//! 字体管理：启动时加载 `gcodeFonts` 目录下全部 `.gfont` 字库。

use std::path::Path;
use wrobot_core::font::{BuiltinFont, FontStack, GfontFile, StrokeFont};
use wrobot_core::doc::Document;

/// 已加载单线字库的集合。
#[derive(Default)]
pub struct FontManager {
    pub fonts: Vec<GfontFile>,
}

impl FontManager {
    /// 从目录加载全部 `.gfont`（失败的单字库会被跳过）。
    pub fn load(dir: &Path) -> Self {
        let mut fonts = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                let is_gfont = p
                    .extension()
                    .map(|e| e.eq_ignore_ascii_case("gfont"))
                    .unwrap_or(false);
                if is_gfont {
                    match GfontFile::open(&p) {
                        Ok(f) => fonts.push(f),
                        Err(e) => eprintln!("跳过字库 {}: {}", p.display(), e),
                    }
                }
            }
        }
        fonts.sort_by(|a, b| a.name().cmp(b.name()));
        FontManager { fonts }
    }

    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }

    pub fn names(&self) -> Vec<String> {
        self.fonts.iter().map(|f| f.name().to_string()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&GfontFile> {
        self.fonts.iter().find(|f| f.name() == name)
    }

    pub fn first_name(&self) -> Option<String> {
        self.fonts.first().map(|f| f.name().to_string())
    }
}

/// 依据工程引用到的字体名，构建 `FontStack`（仅含被引用的字库 + 内置兜底）。
///
/// 这样同一个字会优先命中元素 `fontPath` 指定的字库，保证「逐元素选字体」生效；
/// 内置 `BuiltinFont` 作为最后兜底（ASCII / 占位框）。
pub fn build_stack(mgr: &FontManager, doc: &Document) -> FontStack {
    let mut names: Vec<String> = Vec::new();
    for it in &doc.items {
        if let Some(pi) = &it.page_info {
            for f in &pi.font_path {
                if !names.iter().any(|n| n == f) {
                    names.push(f.clone());
                }
            }
        }
    }
    let mut fonts: Vec<Box<dyn StrokeFont>> = Vec::new();
    for n in &names {
        if let Some(g) = mgr.get(n) {
            fonts.push(Box::new(g.clone()));
        }
    }
    fonts.push(Box::new(BuiltinFont::new()));
    FontStack::new(fonts)
}
