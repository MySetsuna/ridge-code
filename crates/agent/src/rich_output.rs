//! # rich_output —— 富文本输出和媒体支持模块
//!
//! 为 ridge agent 提供:
//! - 彩色格式化输出（ANSI 颜色）
//! - 表格和结构化展示
//! - 图片/视频/文件路径的直接展示
//! - 交互式输出增强

use std::path::Path;

/// 颜色枚举用于 ANSI 颜色代码
#[derive(Debug, Clone, Copy)]
pub enum Color {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    Black,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    /// 灰(ANSI 90)—— 状态行等次要信息用,不抢眼。
    BrightBlack,
}

impl Color {
    /// 获取 ANSI 颜色代码
    pub fn code(&self) -> &'static str {
        match self {
            Color::Red => "\x1b[31m",
            Color::Green => "\x1b[32m",
            Color::Yellow => "\x1b[33m",
            Color::Blue => "\x1b[34m",
            Color::Magenta => "\x1b[35m",
            Color::Cyan => "\x1b[36m",
            Color::White => "\x1b[37m",
            Color::Black => "\x1b[30m",
            Color::BrightRed => "\x1b[31;1m",
            Color::BrightGreen => "\x1b[32;1m",
            Color::BrightYellow => "\x1b[33;1m",
            Color::BrightBlue => "\x1b[34;1m",
            Color::BrightMagenta => "\x1b[35;1m",
            Color::BrightCyan => "\x1b[36;1m",
            Color::BrightWhite => "\x1b[37;1m",
            Color::BrightBlack => "\x1b[90m",
        }
    }

    /// 重置颜色代码
    pub fn reset() -> &'static str {
        "\x1b[0m"
    }
}

/// 富文本输出器，支持颜色和格式
pub struct RichOutput {
    pub color: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl RichOutput {
    /// 创建新的富文本输出器
    pub fn new() -> Self {
        Self {
            color: None,
            bold: false,
            italic: false,
            underline: false,
        }
    }

    /// 设置颜色
    pub fn with_color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// 设置粗体
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// 设置斜体
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// 设置下划线
    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// 生成 ANSI 格式化字符串
    pub fn format(&self, text: &str) -> String {
        let mut codes = String::new();

        if let Some(color) = self.color {
            codes.push_str(color.code());
        }

        if self.bold {
            codes.push_str("\x1b[1m");
        }

        if self.italic {
            codes.push_str("\x1b[3m");
        }

        if self.underline {
            codes.push_str("\x1b[4m");
        }

        format!("{}{}{}", codes, text, Color::reset())
    }

    /// 直接打印格式化文本
    pub fn print(&self, text: &str) {
        println!("{}", self.format(text));
    }

    /// 打印带前缀的消息
    pub fn print_with_prefix(&self, prefix: &str, text: &str) {
        let formatted_prefix = self.format(prefix);
        println!("{} {}", formatted_prefix, text);
    }
}

impl Default for RichOutput {
    fn default() -> Self {
        Self::new()
    }
}

/// 媒体类型枚举
#[derive(Debug, Clone, PartialEq)]
pub enum MediaType {
    Image,
    Video,
    Audio,
    File,
    Directory,
}

/// 媒体信息结构
#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub media_type: MediaType,
    pub path: String,
    pub size: Option<u64>,
    pub description: Option<String>,
}

impl MediaInfo {
    /// 从文件路径创建媒体信息
    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        let path = path.as_ref();
        let metadata = path.metadata().ok()?;

        let media_type = if path.is_dir() {
            MediaType::Directory
        } else {
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("")
                .to_lowercase();

            match extension.as_str() {
                "jpg" | "jpeg" | "png" | "gif" | "bmp" | "svg" | "webp" => MediaType::Image,
                "mp4" | "avi" | "mov" | "wmv" | "flv" | "webm" => MediaType::Video,
                "mp3" | "wav" | "flac" | "aac" | "ogg" => MediaType::Audio,
                _ => MediaType::File,
            }
        };

        Some(Self {
            media_type,
            path: path.display().to_string(),
            size: Some(metadata.len()),
            description: None,
        })
    }

    /// 设置描述
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// 媒体展示器
pub struct MediaDisplay {
    pub output: RichOutput,
}

impl MediaDisplay {
    /// 创建新的媒体展示器
    pub fn new() -> Self {
        Self {
            output: RichOutput::new(),
        }
    }

    /// 展示媒体信息
    pub fn display(&self, media: &MediaInfo) {
        let (icon, label, show_size) = Self::media_presentation(&media.media_type);
        self.output
            .print_with_prefix(icon, &format!("{label}: {}", media.path));
        if show_size {
            if let Some(size) = media.size {
                self.output
                    .print_with_prefix("📏", &format!("大小: {size} bytes"));
            }
        }
        if let Some(description) = &media.description {
            self.output.print_with_prefix("📝", description);
        }
        self.output.print_with_prefix(
            "🔗",
            &format!("路径: \x60file://{}{}\x60", media.path, Color::reset()),
        );
    }

    fn media_presentation(media_type: &MediaType) -> (&'static str, &'static str, bool) {
        match media_type {
            MediaType::Image => ("🖼️", "图片", true),
            MediaType::Video => ("🎬", "视频", true),
            MediaType::Audio => ("🎵", "音频", true),
            MediaType::File => ("📄", "文件", true),
            MediaType::Directory => ("📁", "目录", false),
        }
    }
}

impl Default for MediaDisplay {
    fn default() -> Self {
        Self::new()
    }
}

/// 表格展示器
pub struct TableDisplay {
    pub output: RichOutput,
}

impl TableDisplay {
    /// 创建新的表格展示器
    pub fn new() -> Self {
        Self {
            output: RichOutput::new(),
        }
    }

    /// 展示简单的表格
    pub fn display(&self, headers: &[&str], rows: &[Vec<String>]) {
        if rows.is_empty() {
            self.output.print("📊 表格为空");
            return;
        }

        // 计算列宽
        let col_widths: Vec<usize> = (0..headers.len())
            .map(|i| {
                let header_len = headers[i].len();
                let max_row_len = rows
                    .iter()
                    .map(|row| row.get(i).map(|s| s.len()).unwrap_or(0))
                    .max()
                    .unwrap_or(0);
                header_len.max(max_row_len)
            })
            .collect();

        // 打印表头
        let header_line: String = headers
            .iter()
            .enumerate()
            .map(|(i, header)| format!("{:<width$}", header, width = col_widths[i]))
            .collect();
        self.output.print(&header_line);

        // 打印分隔线
        let separator: String = col_widths
            .iter()
            .map(|&width| "-".repeat(width))
            .collect::<Vec<_>>()
            .join(" ");
        self.output.print(&separator);

        // 打印行数据
        for row in rows {
            let row_line: String = row
                .iter()
                .enumerate()
                .map(|(i, cell)| format!("{:<width$}", cell, width = col_widths[i]))
                .collect();
            self.output.print(&row_line);
        }
    }
}

impl Default for TableDisplay {
    fn default() -> Self {
        Self::new()
    }
}

/// 格式化工具
pub struct Formatter;

impl Formatter {
    /// 格式化文件大小
    pub fn format_file_size(bytes: u64) -> String {
        const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
        let mut size = bytes as f64;
        let mut unit_index = 0;

        while size >= 1024.0 && unit_index < UNITS.len() - 1 {
            size /= 1024.0;
            unit_index += 1;
        }

        if unit_index == 0 {
            format!("{} {}", bytes, UNITS[0])
        } else {
            format!("{:.1} {}", size, UNITS[unit_index])
        }
    }

    /// 格式化持续时间
    pub fn format_duration(seconds: u64) -> String {
        if seconds < 60 {
            format!("{}秒", seconds)
        } else if seconds < 3600 {
            format!("{}分钟", seconds / 60)
        } else {
            format!("{}小时", seconds / 3600)
        }
    }

    /// 格式化进度条
    pub fn format_progress(current: u64, total: u64) -> String {
        if total == 0 {
            return "[==========]".to_string();
        }

        const WIDTH: u64 = 10;
        let percentage = (current as f64 / total as f64) * 100.0;
        let filled = (current * WIDTH / total).min(WIDTH) as usize;
        let empty = WIDTH as usize - filled;

        format!(
            "[{}{}] {:.1}%",
            "█".repeat(filled),
            " ".repeat(empty),
            percentage
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Color, Formatter, MediaDisplay, MediaInfo, MediaType, RichOutput, TableDisplay};
    use std::fs;

    #[test]
    fn test_color_formatting() {
        let output = RichOutput::new().with_color(Color::Green).bold();
        let formatted = output.format("Hello");
        assert!(formatted.contains("\x1b[32m")); // Green
        assert!(formatted.contains("\x1b[1m")); // Bold
        assert!(formatted.contains("Hello"));
        assert!(formatted.contains("\x1b[0m")); // Reset
    }

    #[test]
    fn test_media_info_creation() {
        // 创建测试文件
        let test_file = "test_media.jpg";
        fs::write(test_file, "fake image content").unwrap();

        if let Some(media) = MediaInfo::from_path(test_file) {
            assert_eq!(media.media_type, MediaType::Image);
            assert_eq!(media.path, test_file);
            assert!(media.size.is_some());
        }

        fs::remove_file(test_file).ok();
    }

    #[test]
    fn test_table_display() {
        let table = TableDisplay::new();
        let headers = &["Name", "Age", "City"];
        let rows = vec![
            vec![
                "Alice".to_string(),
                "25".to_string(),
                "New York".to_string(),
            ],
            vec!["Bob".to_string(), "30".to_string(), "London".to_string()],
        ];

        // 这个测试主要检查不崩溃
        table.display(headers, &rows);
    }

    #[test]
    fn test_file_size_formatting() {
        assert_eq!(Formatter::format_file_size(500), "500 B");
        assert_eq!(Formatter::format_file_size(1536), "1.5 KB");
        assert_eq!(Formatter::format_file_size(1048576), "1.0 MB");
        assert_eq!(Formatter::format_file_size(1073741824), "1.0 GB");
    }

    #[test]
    fn test_progress_formatting() {
        assert_eq!(Formatter::format_progress(0, 100), "[          ] 0.0%");
        assert_eq!(Formatter::format_progress(50, 100), "[█████     ] 50.0%");
        assert_eq!(Formatter::format_progress(100, 100), "[██████████] 100.0%");
    }

    #[test]
    fn formatting_and_media_paths_cover_all_supported_variants() {
        let colors = [
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::White,
            Color::Black,
            Color::BrightRed,
            Color::BrightGreen,
            Color::BrightYellow,
            Color::BrightBlue,
            Color::BrightMagenta,
            Color::BrightCyan,
            Color::BrightWhite,
            Color::BrightBlack,
        ];
        for color in colors {
            assert!(color.code().starts_with('\x1b'));
        }
        assert_eq!(Color::reset(), "\x1b[0m");
        let rich = RichOutput::new()
            .with_color(Color::Blue)
            .bold()
            .italic()
            .underline();
        let formatted = rich.format("styled");
        assert!(formatted.contains("\x1b[34m"));
        assert!(formatted.contains("\x1b[3m"));
        assert!(formatted.contains("\x1b[4m"));

        assert_eq!(Formatter::format_duration(30), "30秒");
        assert_eq!(Formatter::format_duration(120), "2分钟");
        assert_eq!(Formatter::format_duration(7200), "2小时");
        assert_eq!(
            Formatter::format_file_size(1024 * 1024 * 1024 * 1024),
            "1.0 TB"
        );
        assert_eq!(Formatter::format_progress(1, 0), "[==========]");

        let root = std::env::temp_dir().join(format!("ridge-media-{}", std::process::id()));
        let _ = fs::create_dir_all(&root);
        let paths = ["photo.png", "clip.mp4", "sound.mp3", "notes.txt"];
        let expected = [
            MediaType::Image,
            MediaType::Video,
            MediaType::Audio,
            MediaType::File,
        ];
        for (name, media_type) in paths.into_iter().zip(expected) {
            let path = root.join(name);
            fs::write(&path, "data").unwrap();
            let media = MediaInfo::from_path(&path)
                .unwrap()
                .with_description("demo");
            assert_eq!(media.media_type, media_type);
            MediaDisplay::new().display(&media);
        }
        let directory = MediaInfo::from_path(&root).unwrap();
        assert_eq!(directory.media_type, MediaType::Directory);
        MediaDisplay::new().display(&directory);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_and_missing_display_inputs_are_handled_without_panics() {
        assert!(MediaInfo::from_path("ridge-media-does-not-exist").is_none());
        let output = RichOutput::new().with_color(Color::BrightBlack);
        output.print("plain");
        output.print_with_prefix("prefix", "text");
        TableDisplay::new().display(&["Name"], &[]);
    }
}
