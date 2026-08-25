import { existsSync, readFileSync, readdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join, relative, resolve } from "node:path";
import YAML from "yaml";
import {
  buildGraph,
  buildObsidianProjection,
  parseProject,
  validateGraph,
} from "@jackjiang18/spectree";

const root = resolve(process.argv[2] ?? process.cwd());
const vault = resolve(process.argv[3] ?? join(root, "obsidian-vault"));
const parsed = parseProject(root);
const built = buildGraph(parsed);
const validation = validateGraph(built.graph, built.diagnostics);
const projection = buildObsidianProjection(built.graph);
const errors = validation.diagnostics
  .filter((diagnostic) => diagnostic.severity === "ERROR")
  .map((diagnostic) => `${diagnostic.code}: ${diagnostic.message}`);
if (built.graph.nodes.size === 0) {
  errors.push("SpecTree graph is empty");
}

const targetOwners = new Set();
for (const node of built.graph.nodes.values()) {
  for (const target of [...node.codeTargets, ...node.testTargets]) {
    targetOwners.add(target.replaceAll("\\", "/"));
  }
}

function targetRegExp(target) {
  const escaped = target
    .replaceAll("\\", "/")
    .replace(/[.+^${}()|[\]\\]/g, "\\$&")
    .replaceAll("*", ".*");
  return new RegExp(`^${escaped}$`);
}

const targetPatterns = [...targetOwners].map((target) => [target, targetRegExp(target)]);
const mappedBySpec = (file) => {
  const normalized = file.replaceAll("\\", "/");
  return targetPatterns.some(([target, pattern]) =>
    target.includes("*") ? pattern.test(normalized) : target === normalized,
  );
};

const generatedSpectreeState = (file) =>
  file === ".spectree/approvals.json" ||
  file === ".spectree/evidence.json" ||
  file === ".spectree/need-spec-change.json" ||
  file === ".spectree/spectree.lock.json" ||
  file.startsWith(".spectree/build/");

function sourceFiles(directory) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if ([".git", ".ridge", "target"].includes(entry.name)) continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...sourceFiles(path));
    } else if (entry.isFile() && entry.name.endsWith(".rs")) {
      files.push(path);
    }
  }
  return files;
}

const withinRoot = (candidate, parent) => {
  const relativePath = relative(parent, candidate);
  return relativePath === "" || (!relativePath.startsWith("..") && !/^[A-Za-z]:/.test(relativePath));
};

let targetCount = 0;
for (const node of built.graph.nodes.values()) {
  for (const target of [...node.codeTargets, ...node.testTargets]) {
    targetCount += 1;
    const candidate = resolve(root, target);
    if (!withinRoot(candidate, root) || (!target.includes("*") && !existsSync(candidate))) {
      errors.push(`${node.id}: missing target ${target}`);
    }
  }
}

const gitOutput = (args) => {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  return result.status === 0 ? result.stdout.split(/\r?\n/).filter(Boolean) : [];
};
const changedPaths = new Set([
  ...gitOutput(["diff", "--name-only"]),
  ...gitOutput(["ls-files", "--others", "--exclude-standard"]),
]);
for (const file of changedPaths) {
  const normalized = file.replaceAll("\\", "/");
  const isScopedImplementation =
    normalized.startsWith("crates/") &&
    !normalized.includes("/.ridge/") &&
    !normalized.includes("/target/");
  const isScopedTooling =
    normalized === "Cargo.toml" ||
    normalized === "package.json" ||
    normalized === "package-lock.json" ||
    normalized.startsWith("scripts/") ||
    normalized.startsWith(".spectree/") && !generatedSpectreeState(normalized);
  if ((isScopedImplementation || isScopedTooling) && !mappedBySpec(normalized)) {
    errors.push(`changed path is not mapped by any spec target: ${normalized}`);
  }
}

const cratesRoot = join(root, "crates");
let rustSourceCount = 0;
if (existsSync(cratesRoot)) {
  for (const file of sourceFiles(cratesRoot)) {
    rustSourceCount += 1;
    const normalized = relative(root, file).replaceAll("\\", "/");
    if (!mappedBySpec(normalized)) {
      errors.push(`Rust source is not mapped by any spec target: ${normalized}`);
    }
  }
}

const managedNotes = new Set();
for (const note of projection.notes) {
  const target = resolve(vault, ...note.relativePath.split("/"));
  managedNotes.add(note.relativePath.replaceAll("/", "\\"));
  if (!withinRoot(target, vault) || !existsSync(target)) {
    errors.push(`${note.id}: missing vault note ${note.relativePath}`);
    continue;
  }
  const content = readFileSync(target, "utf8");
  const match = content.match(/^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/);
  if (!match) {
    errors.push(`${note.id}: invalid vault frontmatter ${note.relativePath}`);
    continue;
  }
  const frontmatter = YAML.parse(match[1]);
  const source = built.graph.nodes.get(note.id) ?? built.graph.changes.get(note.id);
  if (!source || frontmatter?.id !== note.id) {
    errors.push(`${note.id}: vault id mismatch ${note.relativePath}`);
  }
  if (frontmatter?.graphHash !== built.graph.hash) {
    errors.push(`${note.id}: graphHash drift in ${note.relativePath}`);
  }
  if (source && frontmatter?.sourceHash !== source.sourceHash) {
    errors.push(`${note.id}: sourceHash drift in ${note.relativePath}`);
  }
  if (!["zh-CN", "en-US"].includes(frontmatter?.overview_language)) {
    errors.push(`${note.id}: missing or invalid overview_language in ${note.relativePath}`);
  }
  if (source && frontmatter?.overview_detail_level !== source.level) {
    errors.push(`${note.id}: overview_detail_level mismatch in ${note.relativePath}`);
  }
  if (frontmatter?.overview_includes_children !== true) {
    errors.push(`${note.id}: overview_includes_children must be true in ${note.relativePath}`);
  }
  const body = content.slice(match[0].length);
  if (!/^## (?:节点概览|Node overview)$/mu.test(body)) {
    errors.push(`${note.id}: missing generated node overview in ${note.relativePath}`);
  }
  if (source && built.graph.nodes.has(note.id)) {
    const children = built.graph.edges
      .filter((edge) => edge.type === "parent" && edge.from === note.id)
      .map((edge) => edge.to)
      .sort();
    for (const child of children) {
      if (!body.includes(`[[${child}]]`)) {
        errors.push(`${note.id}: overview missing child ${child} in ${note.relativePath}`);
      }
    }
    if (children.length === 0 && !/(?:无（叶节点）|none \(leaf\))/u.test(body)) {
      errors.push(`${note.id}: leaf is not explicit in ${note.relativePath}`);
    }
  }
}

if (existsSync(vault)) {
  for (const entry of readdirSync(vault, { withFileTypes: true })) {
    if (entry.isFile() && entry.name.endsWith(".md") && /^L[1-4]-/.test(entry.name) && !managedNotes.has(entry.name)) {
      errors.push(`unprojected vault note ${entry.name}`);
    }
  }
}

const result = {
  status: errors.length ? "DRIFT" : "ALIGNED",
  graphHash: built.graph.hash,
  nodes: built.graph.nodes.size,
  targets: targetCount,
  rustSources: rustSourceCount,
  notes: projection.notes.length,
  errors,
};
console.log(JSON.stringify(result, null, 2));
if (errors.length) process.exitCode = 1;
