import { readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import {
  buildGraph,
  parseProject,
  projectObsidianVault,
  validateGraph,
} from "@jackjiang18/spectree";

const root = resolve(process.argv[2] ?? process.cwd());
const vault = resolve(process.argv[3] ?? join(root, "obsidian-vault"));
const requestedLanguage = process.argv[4] ?? "auto";
if (!["auto", "zh-CN", "en-US"].includes(requestedLanguage)) {
  throw new Error(`unsupported overview language: ${requestedLanguage}`);
}
const parsed = parseProject(root);
const built = buildGraph(parsed);
const validation = validateGraph(built.graph, built.diagnostics);

if (!validation.valid) {
  for (const diagnostic of validation.diagnostics) {
    console.error(`${diagnostic.severity} ${diagnostic.code} ${diagnostic.message}`);
  }
  process.exitCode = 1;
  throw new Error("SpecTree validation failed");
}

const projection = projectObsidianVault(root, vault);

function replaceField(lines, key, replacement) {
  const start = lines.findIndex((line) => line.startsWith(`${key}:`));
  if (start < 0) return;
  let end = start + 1;
  while (end < lines.length && /^\s+-\s/.test(lines[end])) end += 1;
  lines.splice(start, end - start, ...replacement);
}

function setScalarField(lines, key, value) {
  const replacement = [`${key}: ${value}`];
  const start = lines.findIndex((line) => line.startsWith(`${key}:`));
  if (start < 0) {
    lines.push(...replacement);
    return;
  }
  let end = start + 1;
  while (end < lines.length && /^\s+-\s/.test(lines[end])) end += 1;
  lines.splice(start, end - start, ...replacement);
}

function links(ids) {
  return ids.length ? ids.map((id) => `[[${id}]]`).join(", ") : "_none_";
}

function overviewLanguage(record) {
  if (requestedLanguage !== "auto") return requestedLanguage;
  const sourceUsesHan = /[\u3400-\u9fff]/u.test(
    `${record?.title ?? ""}\n${record?.body ?? ""}`,
  );
  const hostUsesChinese = Intl.DateTimeFormat().resolvedOptions().locale
    .toLowerCase()
    .startsWith("zh");
  return sourceUsesHan || hostUsesChinese ? "zh-CN" : "en-US";
}

function detailLevel(record) {
  const parsed = Number.parseInt(record?.level?.slice(1) ?? "", 10);
  return Number.isFinite(parsed) ? Math.max(1, Math.min(4, parsed)) : 1;
}

function firstParagraph(record) {
  const paragraph = (record?.body ?? "")
    .replace(/^# .*\r?\n/u, "")
    .split(/\r?\n\s*\r?\n/u)
    .map((paragraph) => paragraph.replace(/\s+/gu, " ").trim())
    .find((paragraph) => paragraph && !paragraph.startsWith("#"));
  if (!paragraph) return undefined;
  const characters = [...paragraph];
  return characters.length > 240 ? `${characters.slice(0, 240).join("")}…` : paragraph;
}

function targetSummary(targets) {
  const visible = targets.slice(0, 5).map((target) => `\`${target}\``);
  const remainder = targets.length - visible.length;
  return `${visible.join(", ") || "_none_"}${remainder > 0 ? ` (+${remainder})` : ""}`;
}

function buildOverview(record, children, language) {
  const childRows = children.map((id) => {
    const child = built.graph.nodes.get(id);
    return `  - [[${id}]] — ${child?.title ?? id} (${child?.level ?? "?"})`;
  });
  const level = detailLevel(record);
  const purpose = firstParagraph(record) ?? record.title;
  const codeTargets = record.codeTargets ?? [];
  const testTargets = record.testTargets ?? [];
  const recordLevel = record.level ?? "CHG";
  if (language === "zh-CN") {
    const rows = [
      "## 节点概览",
      "",
      `- 角色：${record.title}`,
      `- 层级/状态：${record.level} / ${record.status}`,
      `- 目的：${purpose}`,
      children.length ? "- 直接子节点：" : "- 直接子节点：无（叶节点）",
      ...childRows,
    ];
    if (level >= 2) {
      rows.push(`- 实施范围：${targetSummary(record.codeTargets)}`);
      rows.push(`- 测试范围：${targetSummary(record.testTargets)}`);
    }
    if (level >= 3) {
      const gaps = Array.isArray(record.fields.known_gap) ? record.fields.known_gap.length : 0;
      rows.push(`- 验证语境：${record.testTargets.length} 个测试目标；${gaps} 个已知缺口。`);
    }
    if (level >= 4) rows.push(`- 具体工件：${targetSummary(record.codeTargets)}`);
    return rows.join("\n");
  }

  const rows = [
    "## Node overview",
    "",
    `- Role: ${record.title}`,
    `- Level/status: ${record.level} / ${record.status}`,
    `- Purpose: ${purpose}`,
    children.length ? "- Direct children:" : "- Direct children: none (leaf)",
    ...childRows,
  ];
  if (level >= 2) {
    rows.push(`- Implementation scope: ${targetSummary(record.codeTargets)}`);
    rows.push(`- Test scope: ${targetSummary(record.testTargets)}`);
  }
  if (level >= 3) {
    const gaps = Array.isArray(record.fields.known_gap) ? record.fields.known_gap.length : 0;
    rows.push(`- Validation context: ${record.testTargets.length} test targets; ${gaps} known gaps.`);
  }
  if (level >= 4) rows.push(`- Concrete artifacts: ${targetSummary(record.codeTargets)}`);
  return rows.join("\n");
}

for (const note of projection.notes) {
  const target = join(vault, ...note.relativePath.split("/"));
  const content = readFileSync(target, "utf8");
  const match = content.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n([\s\S]*)$/);
  if (!match) throw new Error(`invalid generated note: ${note.relativePath}`);

  const sourceRecord =
    built.graph.nodes.get(note.id) ?? built.graph.changes.get(note.id);
  if (!sourceRecord) throw new Error(`unknown projection record: ${note.id}`);
  const record = {
    ...sourceRecord,
    level: sourceRecord.level ?? "CHG",
    codeTargets: sourceRecord.codeTargets ?? [],
    testTargets: sourceRecord.testTargets ?? [],
  };
  const parent = record?.parent;
  const children = built.graph.edges
    .filter((edge) => edge.type === "parent" && edge.from === note.id)
    .map((edge) => edge.to)
    .sort();
  const frontmatter = match[1].split(/\r?\n/);
  replaceField(frontmatter, "parent", parent ? [`parent: ${parent}`] : []);
  replaceField(
    frontmatter,
    "parent_link",
    parent ? [`parent_link: "[[${parent}]]"`] : [],
  );
  replaceField(
    frontmatter,
    "children",
    children.length ? ["children:", ...children.map((id) => `  - ${id}`)] : ["children: []"],
  );
  replaceField(
    frontmatter,
    "children_links",
    children.length
      ? ["children_links:", ...children.map((id) => `  - "[[${id}]]"`)]
      : ["children_links: []"],
  );
  const language = overviewLanguage(record);
  setScalarField(frontmatter, "overview_language", language);
  if (sourceRecord.level) {
    setScalarField(frontmatter, "overview_detail_level", sourceRecord.level);
  }
  setScalarField(frontmatter, "overview_includes_children", "true");

  let body = match[2].replace(/^\r?\n/, "");
  if (record?.title) {
    const heading = `# ${record.title}`;
    const doubledHeading = `${heading}\n\n${heading}\n\n`;
    if (body.startsWith(doubledHeading)) body = `${heading}\n\n${body.slice(doubledHeading.length)}`;
  }
  body = body
    .replace(/^- Parent:.*$/m, `- Parent: ${links(parent ? [parent] : [])}`)
    .replace(/^- Children:.*$/m, `- Children: ${links(children)}`);
  const heading = `# ${record.title}`;
  const overview = buildOverview(record, children, language);
  if (body.startsWith(heading)) {
    body = `${heading}\n\n${overview}\n\n${body.slice(heading.length).trimStart()}`;
  } else {
    body = `${overview}\n\n${body}`;
  }
  writeFileSync(target, `---\n${frontmatter.join("\n")}\n---\n\n${body.trim()}\n`, "utf8");
}

console.log(
  JSON.stringify(
    {
      status: "EXPORTED",
      vault,
      graphHash: projection.graphHash,
      notes: projection.notes.length,
      overviewLanguage: requestedLanguage,
    },
    null,
    2,
  ),
);
