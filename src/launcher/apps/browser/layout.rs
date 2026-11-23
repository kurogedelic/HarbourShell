//! Layout: DOM -> a flat, y-sorted list of drawable items for a fixed viewport width.
//!
//! Text is M+ 12 (Latin 6 px, Japanese 12 px, from [`Metrics`]) wrapped greedily; Japanese wraps
//! between characters with simple kinsoku. Headings are bold (double-strike) and underlined,
//! italics are dotted-underlined, links are underlined. Tables become columns when they fit,
//! else stack. Everything here is pure (no SDL), so it runs in a worker thread and is unit tested.

use std::collections::HashMap;

use super::html::{Dom, Kind, NodeId};
use super::url;

pub const LINE_H: i32 = 14;
pub const BLOCK_GAP: i32 = 6;
pub const MAX_ITEMS: usize = 40_000;
pub const MAX_HEIGHT: i32 = 60_000;
pub const INDENT: i32 = 14;

/// Character widths of the fonts (they are fixed-pitch), so layout needs no SDL font.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub latin: i32,
    pub wide: i32,
    pub jis: bool,
}

impl Metrics {
    pub const fn fixed(latin: i32, wide: i32) -> Self {
        Metrics { latin, wide, jis: true }
    }

    pub fn is_wide(&self, ch: char) -> bool {
        self.jis && (ch as u32) >= 0x80 && crate::jis::unicode_to_jis(ch).is_some()
    }

    pub fn char_w(&self, ch: char) -> i32 {
        if self.is_wide(ch) { self.wide } else { self.latin }
    }

    pub fn width(&self, s: &str) -> i32 {
        s.chars().map(|c| self.char_w(c)).sum()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub mono: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Link(u32),
    Field(u32),
    /// A placeholder picture (no link of its own).
    Image(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    Text { text: String, style: Style },
    Rule,
    /// Blockquote bar.
    Bar,
    /// Background band (pre blocks).
    Bg,
    Img(u32),
    Field(u32),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub kind: ItemKind,
    pub link: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    Text,
    Password,
    Submit,
    Check,
    Radio,
    Select,
    Hidden,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub kind: FieldKind,
    pub name: String,
    pub value: String,
    pub label: String,
    pub checked: bool,
    pub options: Vec<(String, String)>,
    pub sel: usize,
    pub form: Option<usize>,
    pub size: usize,
}

#[derive(Clone, Debug)]
pub struct Form {
    pub action: String,
    pub post: bool,
    pub fields: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct ImageRef {
    pub url: String,
    pub alt: String,
    /// The box reserved in the layout (0 x 0 = unknown size: only a placeholder).
    pub box_w: i32,
    pub box_h: i32,
    /// Size unknown: only a one-line `[alt]` box, opened full screen on demand.
    pub placeholder: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Get(String),
    Post { url: String, body: String },
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub items: Vec<Item>,
    pub height: i32,
    /// Resolved link targets.
    pub links: Vec<String>,
    pub forms: Vec<Form>,
    pub fields: Vec<Field>,
    pub images: Vec<ImageRef>,
    pub anchors: HashMap<String, i32>,
    /// Tab order: links and fields in document order, with the y of their first item.
    pub focus: Vec<(Focus, i32, i32)>,
    pub refresh: Option<String>,
    pub truncated: bool,
    /// Laid out by reader mode (only the main text).
    pub reader: bool,
    /// The HTML this page was made from (kept so reader mode can re-lay it out).
    pub source: Option<std::sync::Arc<String>>,
    /// For the "Page info" card: how the bytes were decoded, how many frames were stacked, download size.
    pub charset: String,
    pub frames: usize,
    pub bytes: usize,
}

impl Page {
    /// Index range of items that may be visible in `top..bottom` (document y).
    pub fn visible(&self, top: i32, bottom: i32) -> std::ops::Range<usize> {
        let a = self.items.partition_point(|i| i.y + MAX_ITEM_H < top);
        let b = self.items.partition_point(|i| i.y < bottom);
        a..b.max(a)
    }

    /// What is under the document point (x, y).
    pub fn hit(&self, x: i32, y: i32) -> Option<Focus> {
        for i in self.visible(y, y + 1) {
            let it = &self.items[i];
            if x >= it.x && x < it.x + it.w && y >= it.y && y < it.y + it.h {
                match it.kind {
                    ItemKind::Field(f) => return Some(Focus::Field(f)),
                    ItemKind::Img(i) if it.link.is_none() && self.images[i as usize].placeholder => return Some(Focus::Image(i)),
                    _ => {}
                }
                if let Some(l) = it.link {
                    return Some(Focus::Link(l));
                }
            }
        }
        None
    }

    /// Add another page below this one (frames shown one after another). Indices of links, fields,
    /// forms and images are shifted so everything keeps working.
    pub fn append(&mut self, o: Page, gap: i32) {
        let (dy, lo, fo, fm, io) = (self.height + gap, self.links.len() as u32, self.fields.len() as u32, self.forms.len(), self.images.len() as u32);
        let gap = if self.items.is_empty() { 0 } else { gap };
        let dy = if gap == 0 { self.height } else { dy };
        if gap > 0 {
            self.items.push(Item { x: 0, y: self.height + gap / 2, w: 304, h: 1, kind: ItemKind::Rule, link: None });
        }
        for mut it in o.items {
            it.y += dy;
            it.link = it.link.map(|l| l + lo);
            it.kind = match it.kind {
                ItemKind::Img(i) => ItemKind::Img(i + io),
                ItemKind::Field(f) => ItemKind::Field(f + fo),
                k => k,
            };
            self.items.push(it);
        }
        self.links.extend(o.links);
        for mut f in o.fields {
            f.form = f.form.map(|x| x + fm);
            self.fields.push(f);
        }
        for mut f in o.forms {
            f.fields.iter_mut().for_each(|x| *x += fo as usize);
            self.forms.push(f);
        }
        self.images.extend(o.images);
        for (k, y) in o.anchors {
            self.anchors.entry(k).or_insert(y + dy);
        }
        for (f, y, h) in o.focus {
            let f = match f {
                Focus::Link(l) => Focus::Link(l + lo),
                Focus::Field(x) => Focus::Field(x + fo),
                Focus::Image(i) => Focus::Image(i + io),
            };
            self.focus.push((f, y + dy, h));
        }
        self.height = dy + o.height;
        self.truncated |= o.truncated;
    }

    /// Where this field sits (first item).
    pub fn field_rect(&self, f: u32) -> Option<(i32, i32, i32, i32)> {
        self.items.iter().find(|i| i.kind == ItemKind::Field(f)).map(|i| (i.x, i.y, i.w, i.h))
    }

    /// Fields to send for `form` when `submit` was pressed.
    pub fn form_fields(&self, form: usize, submit: Option<usize>) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for &fi in &self.forms[form].fields {
            let f = &self.fields[fi];
            if f.name.is_empty() {
                continue;
            }
            match f.kind {
                FieldKind::Text | FieldKind::Password | FieldKind::Hidden => out.push((f.name.clone(), f.value.clone())),
                FieldKind::Check | FieldKind::Radio if f.checked => out.push((f.name.clone(), if f.value.is_empty() { "on".into() } else { f.value.clone() })),
                FieldKind::Select => {
                    if let Some((_, v)) = f.options.get(f.sel) {
                        out.push((f.name.clone(), v.clone()));
                    }
                }
                FieldKind::Submit if Some(fi) == submit => out.push((f.name.clone(), f.value.clone())),
                _ => {}
            }
        }
        out
    }

    pub fn form_request(&self, form: usize, submit: Option<usize>) -> Option<Request> {
        let fields = self.form_fields(form, submit);
        let f = &self.forms[form];
        if f.post {
            let target = url::resolve(&self.url, &f.action)?;
            Some(Request::Post { url: url::strip_fragment(&target).to_string(), body: url::form_body(&fields) })
        } else {
            url::form_get_url(&self.url, &f.action, &fields).map(Request::Get)
        }
    }

    /// Toggle a check box / radio (radios clear their group).
    pub fn toggle(&mut self, f: usize) {
        match self.fields[f].kind {
            FieldKind::Check => self.fields[f].checked = !self.fields[f].checked,
            FieldKind::Radio => {
                let (form, name) = (self.fields[f].form, self.fields[f].name.clone());
                for (i, g) in self.fields.iter_mut().enumerate() {
                    if g.kind == FieldKind::Radio && g.form == form && g.name == name {
                        g.checked = i == f;
                    }
                }
            }
            FieldKind::Select => {
                let n = self.fields[f].options.len().max(1);
                self.fields[f].sel = (self.fields[f].sel + 1) % n;
            }
            _ => {}
        }
    }
}

/// An item is never taller than this, so `visible` can binary-search by y.
pub const MAX_ITEM_H: i32 = 130;

// ------------------------------------------------------------------------------- layout

enum SegKind {
    Text(String),
    Field(u32),
    Img(u32),
}

struct Seg {
    kind: SegKind,
    x: i32,
    w: i32,
    h: i32,
    style: Style,
    link: Option<u32>,
}

struct L<'a> {
    dom: &'a Dom,
    m: Metrics,
    base: String,
    p: Page,
    y: i32,
    left: i32,
    right: i32,
    gap: i32,
    line: Vec<Seg>,
    line_w: i32,
    pending_space: bool,
    center: bool,
    pre: bool,
    style: Style,
    link: Option<u32>,
    marker: Option<String>,
    form: Option<usize>,
    lists: Vec<(bool, u32)>,
    seen_link: Vec<bool>,
    table_depth: u32,
    full: bool,
    reader: bool,
}

fn is_block(tag: &str) -> bool {
    matches!(tag, "p" | "div" | "section" | "article" | "header" | "footer" | "nav" | "aside" | "main" | "ul" | "ol" | "li" | "dl" | "dt" | "dd" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "pre" | "hr" | "table" | "tr" | "form" | "fieldset" | "address" | "figure" | "figcaption" | "details" | "summary" | "body" | "html" | "center" | "caption" | "legend" | "tbody" | "thead" | "tfoot" | "td" | "th")
}

fn is_opening(c: char) -> bool {
    "「『（［｛〈《【〔“‘(".contains(c)
}

fn is_closing(c: char) -> bool {
    "、。，．・：；？！）］｝〉》」』】〕ー々ゝゞぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮ”’!?,.;:)".contains(c)
}

/// Replace characters the fonts cannot draw (they would show as boxes) with look-alikes.
pub fn fold(m: &Metrics, s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\u{2014}' | '\u{2013}' | '\u{2015}' | '\u{2212}' | '\u{2010}' | '\u{2011}' => out.push('-'),
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{2032}' => out.push('\''),
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2033}' => out.push('"'),
            '\u{2026}' => out.push_str("..."),
            '\u{2022}' | '\u{25CF}' | '\u{25AA}' | '\u{25E6}' | '\u{00B7}' => out.push('*'),
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2009}' | '\u{200A}' | '\u{3000}' if !m.is_wide(ch) || ch == '\u{3000}' => out.push('\u{00A0}'),
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{00AD}' => {}
            c if (c as u32) > 0xFF && !m.is_wide(c) => out.push('?'),
            c => out.push(c),
        }
    }
    out
}

/// Break text into units that may wrap between them. Whitespace units are " ".
pub fn tokens(m: &Metrics, s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_wide = false;
    let mut after_open = false;
    for ch in s.chars() {
        if ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if out.last().map(|l| l != " ").unwrap_or(true) {
                out.push(" ".into());
            }
            after_open = false;
            continue;
        }
        let wide = m.is_wide(ch);
        if wide {
            if is_closing(ch) && !cur.is_empty() {
                cur.push(ch);
                cur_wide = true;
                continue;
            }
            if !(after_open && cur_wide) && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(ch);
            cur_wide = true;
            after_open = is_opening(ch);
            if !after_open {
                // a wide char is its own unit (closing punctuation may still attach)
            }
        } else {
            if cur_wide && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur_wide = false;
            after_open = false;
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl<'a> L<'a> {
    fn avail(&self) -> i32 {
        (self.right - self.left).max(self.m.wide)
    }

    fn mk_item(&mut self, it: Item) {
        if self.p.items.len() >= MAX_ITEMS || it.y > MAX_HEIGHT {
            self.full = true;
            return;
        }
        self.p.items.push(it);
    }

    fn apply_gap(&mut self) {
        if self.y > 0 {
            self.y += self.gap;
        }
        self.gap = 0;
    }

    fn block_edge(&mut self, gap: i32) {
        self.flush_line();
        self.gap = self.gap.max(gap);
    }

    fn flush_line(&mut self) {
        // trim a trailing space
        if let Some(Seg { kind: SegKind::Text(t), w, .. }) = self.line.last_mut() {
            if t.ends_with(' ') {
                t.pop();
                *w -= self.m.latin;
                self.line_w -= self.m.latin;
            }
        }
        self.pending_space = false;
        if self.line.is_empty() {
            return;
        }
        self.apply_gap();
        let lh = self.line.iter().map(|s| s.h).max().unwrap_or(LINE_H).max(LINE_H);
        let off = if self.center { ((self.avail() - self.line_w) / 2).max(0) } else { 0 };
        let segs = std::mem::take(&mut self.line);
        if let Some(mk) = self.marker.take() {
            let w = self.m.width(&mk);
            self.mk_item(Item { x: self.left - INDENT + (INDENT - w - 2).max(0), y: self.y + (lh - LINE_H) / 2, w, h: LINE_H, kind: ItemKind::Text { text: mk, style: Style::default() }, link: None });
        }
        for s in segs {
            let y = self.y + (lh - s.h) / 2;
            let x = self.left + off + s.x;
            let kind = match s.kind {
                SegKind::Text(t) => ItemKind::Text { text: t.replace('\u{A0}', " "), style: s.style },
                SegKind::Field(f) => ItemKind::Field(f),
                SegKind::Img(i) => ItemKind::Img(i),
            };
            self.register_focus(&kind, s.link, y, s.h);
            self.mk_item(Item { x, y, w: s.w, h: s.h, kind, link: s.link });
        }
        self.y += lh;
        self.line_w = 0;
    }

    fn register_focus(&mut self, kind: &ItemKind, link: Option<u32>, y: i32, h: i32) {
        if let ItemKind::Field(f) = kind {
            self.p.focus.push((Focus::Field(*f), y, h));
        }
        if let ItemKind::Img(i) = kind {
            if link.is_none() && self.p.images[*i as usize].placeholder {
                self.p.focus.push((Focus::Image(*i), y, h));
            }
        }
        if let Some(l) = link {
            if !self.seen_link[l as usize] {
                self.seen_link[l as usize] = true;
                self.p.focus.push((Focus::Link(l), y, h));
            }
        }
    }

    fn push_text(&mut self, text: &str, width: i32) {
        let (style, link) = (self.style, self.link);
        if let Some(Seg { kind: SegKind::Text(t), w, style: s, link: l, .. }) = self.line.last_mut() {
            if *s == style && *l == link {
                t.push_str(text);
                *w += width;
                self.line_w += width;
                return;
            }
        }
        self.line.push(Seg { kind: SegKind::Text(text.to_string()), x: self.line_w, w: width, h: LINE_H, style, link });
        self.line_w += width;
    }

    fn add_space(&mut self) {
        if self.line.is_empty() || self.line_w == 0 {
            return;
        }
        // a space belongs to the previous text segment (no underline gaps on style change)
        if let Some(Seg { kind: SegKind::Text(t), w, .. }) = self.line.last_mut() {
            if !t.ends_with(' ') {
                t.push(' ');
                *w += self.m.latin;
                self.line_w += self.m.latin;
            }
        } else {
            self.line_w += self.m.latin;
        }
    }

    fn add_text(&mut self, s: &str) {
        let s = &fold(&self.m, s);
        if self.pre {
            self.add_pre(s);
            return;
        }
        let bold = self.style.bold as i32;
        for tok in tokens(&self.m, s) {
            if tok == " " {
                self.pending_space = true;
                continue;
            }
            let w = self.m.width(&tok) + bold;
            let sp = if self.pending_space && self.line_w > 0 { self.m.latin } else { 0 };
            if self.line_w + sp + w > self.avail() && self.line_w > 0 {
                self.flush_line();
                self.pending_space = false;
            }
            if self.pending_space && self.line_w > 0 {
                self.add_space();
            }
            self.pending_space = false;
            if w > self.avail() {
                // longer than a whole line: cut it
                let mut chunk = String::new();
                let mut cw = 0;
                for ch in tok.chars() {
                    let c = self.m.char_w(ch);
                    if self.line_w + cw + c > self.avail() {
                        if !chunk.is_empty() {
                            self.push_text(&chunk, cw);
                            chunk.clear();
                            cw = 0;
                        }
                        self.flush_line();
                    }
                    chunk.push(ch);
                    cw += c;
                }
                if !chunk.is_empty() {
                    self.push_text(&chunk, cw + bold);
                }
            } else {
                self.push_text(&tok, w);
            }
        }
    }

    fn add_pre(&mut self, s: &str) {
        for (n, part) in s.split('\n').enumerate() {
            if n > 0 {
                self.pre_newline();
            }
            let part = part.replace('\t', "    ").replace('\r', "");
            let mut chunk = String::new();
            let mut cw = 0;
            for ch in part.chars() {
                let c = self.m.char_w(ch);
                if self.line_w + cw + c > self.avail() {
                    if !chunk.is_empty() {
                        self.push_text(&chunk, cw);
                        chunk.clear();
                        cw = 0;
                    }
                    self.pre_newline();
                }
                chunk.push(ch);
                cw += c;
            }
            if !chunk.is_empty() {
                self.push_text(&chunk, cw);
            }
        }
    }

    fn pre_newline(&mut self) {
        if self.line.is_empty() {
            self.apply_gap();
            self.y += LINE_H; // an empty line
        } else {
            self.flush_line_keep_spaces();
        }
    }

    fn flush_line_keep_spaces(&mut self) {
        // like flush_line without trimming (pre text keeps its trailing blanks; harmless)
        self.flush_line();
    }

    fn br(&mut self) {
        if self.line.is_empty() {
            self.apply_gap();
            self.y += LINE_H;
        } else {
            self.flush_line();
        }
    }

    fn atom(&mut self, kind: SegKind, w: i32, h: i32) {
        if self.line_w + w > self.avail() && self.line_w > 0 {
            self.flush_line();
        }
        if self.pending_space && self.line_w > 0 {
            self.add_space();
        }
        self.pending_space = false;
        let x = self.line_w;
        self.line.push(Seg { kind, x, w, h, style: self.style, link: self.link });
        self.line_w += w + 2;
    }

    fn attr(&self, id: NodeId, n: &str) -> Option<String> {
        self.dom.attr(id, n).map(String::from)
    }

    fn anchor(&mut self, id: NodeId) {
        for n in ["id", "name"] {
            if let Some(v) = self.dom.attr(id, n) {
                if !v.is_empty() {
                    let y = self.y + if self.y > 0 { self.gap } else { 0 };
                    self.p.anchors.entry(v.to_string()).or_insert(y);
                }
            }
        }
    }

    fn reader_skip(&self, id: NodeId, tag: &str) -> bool {
        reader_skips(self.dom, id, tag)
    }

    fn walk_children(&mut self, id: NodeId) {
        let kids = self.dom.nodes[id].children.clone();
        for c in kids {
            if self.full {
                return;
            }
            self.walk(c);
        }
    }

    fn walk(&mut self, id: NodeId) {
        let tag = match &self.dom.nodes[id].kind {
            Kind::Text(t) => {
                let t = t.clone();
                self.add_text(&t);
                return;
            }
            Kind::Elem { tag, .. } => tag.clone(),
        };
        if self.dom.attr(id, "hidden").is_some() || self.dom.attr(id, "style").map(|st| st.replace(' ', "").to_ascii_lowercase().contains("display:none")).unwrap_or(false) {
            return;
        }
        if self.reader && self.reader_skip(id, &tag) {
            return;
        }
        self.anchor(id);
        let saved_style = self.style;
        let saved_link = self.link;
        match tag.as_str() {
            "br" => self.br(),
            "hr" => {
                self.block_edge(4);
                self.apply_gap();
                let (x, w, y) = (self.left, self.avail(), self.y + 3);
                self.mk_item(Item { x, y, w, h: 2, kind: ItemKind::Rule, link: None });
                self.y += 8;
                self.gap = 2;
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.block_edge(BLOCK_GAP + 2);
                self.style.bold = tag.as_str() <= "h3";
                self.style.underline = tag.as_str() >= "h2" && tag.as_str() != "h3";
                if tag == "h4" || tag == "h5" || tag == "h6" {
                    self.style.bold = false;
                    self.style.underline = true;
                }
                let c = self.center;
                self.center = c || self.dom.attr(id, "align") == Some("center");
                self.walk_children(id);
                self.center = c;
                self.block_edge(BLOCK_GAP);
                if tag == "h1" {
                    self.apply_gap();
                    let (x, w, y) = (self.left, self.avail(), self.y);
                    self.mk_item(Item { x, y, w, h: 1, kind: ItemKind::Rule, link: None });
                    self.y += 3;
                }
            }
            "p" | "div" | "section" | "article" | "header" | "footer" | "nav" | "aside" | "main" | "address" | "figure" | "figcaption" | "details" | "summary" | "fieldset" | "legend" | "caption" => {
                let c = self.center;
                if self.dom.attr(id, "align") == Some("center") {
                    self.center = true;
                }
                let gap = if matches!(tag.as_str(), "p" | "fieldset" | "figure") { BLOCK_GAP } else { 0 };
                self.block_edge(gap);
                if tag == "address" {
                    self.style.italic = true;
                }
                if tag == "legend" || tag == "caption" || tag == "summary" {
                    self.style.bold = true;
                }
                self.walk_children(id);
                self.block_edge(gap);
                self.center = c;
            }
            "center" => {
                let c = self.center;
                self.center = true;
                self.block_edge(0);
                self.walk_children(id);
                self.block_edge(0);
                self.center = c;
            }
            "blockquote" => {
                self.block_edge(BLOCK_GAP);
                self.apply_gap();
                let y0 = self.y;
                let (l, r) = (self.left, self.right);
                self.left += INDENT;
                self.walk_children(id);
                self.block_edge(BLOCK_GAP);
                let (x, y1) = (l + 4, self.y);
                self.left = l;
                self.right = r;
                if y1 > y0 {
                    self.mk_item(Item { x, y: y0, w: 1, h: (y1 - y0).min(MAX_ITEM_H), kind: ItemKind::Bar, link: None });
                    // long quotes: more bars so each item stays below MAX_ITEM_H
                    let mut yy = y0 + MAX_ITEM_H;
                    while yy < y1 {
                        self.mk_item(Item { x, y: yy, w: 1, h: (y1 - yy).min(MAX_ITEM_H), kind: ItemKind::Bar, link: None });
                        yy += MAX_ITEM_H;
                    }
                }
            }
            "ul" | "ol" | "dir" | "menu" => {
                self.block_edge(if self.lists.is_empty() { BLOCK_GAP } else { 0 });
                let start = self.dom.attr(id, "start").and_then(|s| s.parse().ok()).unwrap_or(1u32);
                self.lists.push((tag == "ol", start));
                self.walk_children(id);
                self.lists.pop();
                self.block_edge(if self.lists.is_empty() { BLOCK_GAP } else { 0 });
            }
            "li" => {
                self.block_edge(0);
                let (ord, n) = self.lists.last().copied().unwrap_or((false, 0));
                let mark = if ord { format!("{n}.") } else if self.lists.len() > 1 { "-".to_string() } else { "*".to_string() };
                if let Some(l) = self.lists.last_mut() {
                    l.1 += 1;
                }
                self.marker = Some(mark);
                let l = self.left;
                self.left += INDENT;
                self.walk_children(id);
                self.flush_line();
                if self.marker.take().is_some() {
                    // empty item: still show the marker
                }
                self.left = l;
            }
            "dl" => {
                self.block_edge(BLOCK_GAP);
                self.walk_children(id);
                self.block_edge(BLOCK_GAP);
            }
            "dt" => {
                self.block_edge(2);
                self.style.bold = true;
                self.walk_children(id);
                self.block_edge(0);
            }
            "dd" => {
                self.block_edge(0);
                let l = self.left;
                self.left += INDENT;
                self.walk_children(id);
                self.block_edge(2);
                self.left = l;
            }
            "pre" | "listing" | "xmp" | "plaintext" => {
                self.block_edge(BLOCK_GAP);
                self.apply_gap();
                let y0 = self.y;
                let first_item = self.p.items.len();
                self.pre = true;
                self.style.mono = true;
                // a leading newline right after <pre> is ignored
                self.walk_children(id);
                self.pre = false;
                self.flush_line();
                let y1 = self.y;
                // a background band behind the lines (inserted before them so it paints first)
                let mut bands = Vec::new();
                let mut yy = y0;
                while yy < y1 {
                    bands.push(Item { x: self.left - 2, y: yy, w: self.avail() + 4, h: (y1 - yy).min(MAX_ITEM_H), kind: ItemKind::Bg, link: None });
                    yy += MAX_ITEM_H;
                }
                for (k, b) in bands.into_iter().enumerate() {
                    self.p.items.insert(first_item + k, b);
                }
                self.block_edge(BLOCK_GAP);
            }
            "table" => self.table(id),
            "tr" | "tbody" | "thead" | "tfoot" | "td" | "th" | "body" | "html" | "label" => {
                if is_block(&tag) {
                    self.block_edge(0);
                }
                if tag == "th" {
                    self.style.bold = true;
                }
                self.walk_children(id);
                if is_block(&tag) {
                    self.block_edge(0);
                }
            }
            "form" => {
                let fi = self.p.forms.len();
                self.p.forms.push(Form { action: self.attr(id, "action").unwrap_or_default(), post: self.dom.attr(id, "method").map(|m| m.eq_ignore_ascii_case("post")).unwrap_or(false), fields: vec![] });
                let saved = self.form.replace(fi);
                self.block_edge(BLOCK_GAP);
                self.walk_children(id);
                self.block_edge(BLOCK_GAP);
                self.form = saved;
            }
            "b" | "strong" | "th_" => {
                self.style.bold = true;
                self.walk_children(id);
            }
            "i" | "em" | "cite" | "var" | "dfn" => {
                self.style.italic = true;
                self.walk_children(id);
            }
            "u" | "ins" => {
                self.style.underline = true;
                self.walk_children(id);
            }
            "code" | "tt" | "kbd" | "samp" => {
                self.style.mono = true;
                self.walk_children(id);
            }
            "q" => {
                self.add_text("\"");
                self.walk_children(id);
                self.add_text("\"");
            }
            "a" => {
                if let Some(href) = self.dom.attr(id, "href") {
                    if let Some(u) = url::resolve(&self.base, href) {
                        self.p.links.push(u);
                        self.seen_link.push(false);
                        self.link = Some(self.p.links.len() as u32 - 1);
                        self.style.underline = true;
                    }
                }
                self.walk_children(id);
            }
            "img" => self.image(id),
            "input" => self.input(id),
            "button" => self.button(id),
            "textarea" => {
                let v = self.dom.text_of(id);
                self.add_field(id, FieldKind::Text, v, String::new(), vec![], 0);
            }
            "select" => self.select(id),
            "frame" | "frameset" => self.walk_children(id),
            "noframes" => {}
            "meta" => {
                if self.dom.attr(id, "http-equiv").map(|h| h.eq_ignore_ascii_case("refresh")).unwrap_or(false) {
                    if let Some(c) = self.dom.attr(id, "content") {
                        let (delay, rest) = c.split_once(';').unwrap_or((c, ""));
                        let secs = delay.trim().parse::<f32>().unwrap_or(99.0);
                        let l = rest.trim().to_ascii_lowercase();
                        if secs <= 5.0 && l.starts_with("url") {
                            let u = rest.trim()[3..].trim_start_matches(|c: char| c == '=' || c.is_whitespace()).trim_matches(|c| c == '"' || c == '\'');
                            self.p.refresh = url::resolve(&self.base, u);
                        }
                    }
                }
            }
            "title" | "base" | "link" => {}
            _ => self.walk_children(id),
        }
        self.style = saved_style;
        self.link = saved_link;
    }

    fn image(&mut self, id: NodeId) {
        let alt = self.dom.attr(id, "alt").unwrap_or("").trim().to_string();
        let src = self.dom.attr(id, "src").and_then(|s| url::resolve(&self.base, s));
        let Some(src) = src else {
            return;
        };
        let (aw, ah) = (self.dom.attr(id, "width").and_then(|s| s.trim_end_matches("px").parse::<i32>().ok()), self.dom.attr(id, "height").and_then(|s| s.trim_end_matches("px").parse::<i32>().ok()));
        let label = if alt.is_empty() { "image".to_string() } else { alt.clone() };
        let (w, h) = match (aw, ah) {
            (Some(w), Some(h)) if w >= 16 && h >= 12 => {
                let av = self.avail();
                let s = (av as f32 / w as f32).min(100.0 / h as f32).min(1.0);
                (((w as f32 * s) as i32).max(8), ((h as f32 * s) as i32).max(8))
            }
            (Some(w), Some(h)) if w < 16 || h < 12 => return, // trackers, spacers, bullets
            _ => (0, 0),
        };
        let idx = self.p.images.len() as u32;
        self.p.images.push(ImageRef { url: src, alt: label.clone(), box_w: w, box_h: h, placeholder: w == 0 });
        if w == 0 {
            // unknown size: a one-line placeholder box with the alt text
            let shown = fit(&self.m, &format!("[{label}]"), (self.avail() - 8).min(150));
            let tw = self.m.width(&shown) + 4;
            self.atom(SegKind::Img(idx), tw, LINE_H);
            self.p.images[idx as usize].box_w = tw;
            self.p.images[idx as usize].box_h = LINE_H;
            self.p.images[idx as usize].alt = shown;
        } else {
            self.atom(SegKind::Img(idx), w, h.min(MAX_ITEM_H - 2));
        }
    }

    fn add_field(&mut self, id: NodeId, kind: FieldKind, value: String, label: String, options: Vec<(String, String)>, sel: usize) {
        // Controls outside a form cannot be sent anywhere (pages use them for scripts): skip them.
        if self.form.is_none() {
            return;
        }
        let name = self.attr(id, "name").unwrap_or_default();
        let size = self.dom.attr(id, "size").and_then(|s| s.parse::<usize>().ok()).unwrap_or(20).clamp(4, 60);
        let checked = self.dom.attr(id, "checked").is_some();
        let fi = self.p.fields.len();
        self.p.fields.push(Field { kind: kind.clone(), name, value, label, checked, options, sel, form: self.form, size });
        if let Some(f) = self.form {
            self.p.forms[f].fields.push(fi);
        }
        let w = match kind {
            FieldKind::Hidden => return,
            FieldKind::Text | FieldKind::Password => (size as i32 * self.m.latin + 10).min(self.avail()).max(40),
            FieldKind::Submit => self.m.width(&self.p.fields[fi].label) + 12,
            FieldKind::Check | FieldKind::Radio => 12,
            FieldKind::Select => (self.p.fields[fi].options.iter().map(|o| self.m.width(&o.0)).max().unwrap_or(24) + 22).min(self.avail()),
        };
        let h = match kind {
            FieldKind::Check | FieldKind::Radio => 12,
            _ => 16,
        };
        self.atom(SegKind::Field(fi as u32), w.min(self.avail()), h);
    }

    fn input(&mut self, id: NodeId) {
        let ty = self.dom.attr(id, "type").unwrap_or("text").to_ascii_lowercase();
        let value = self.attr(id, "value").unwrap_or_default();
        match ty.as_str() {
            "hidden" => self.add_field(id, FieldKind::Hidden, value, String::new(), vec![], 0),
            "submit" | "button" | "image" | "reset" => {
                let label = if !value.is_empty() { value.clone() } else if ty == "image" { self.attr(id, "alt").unwrap_or("Go".into()) } else { "Submit".into() };
                if ty == "reset" {
                    return;
                }
                self.add_field(id, FieldKind::Submit, value, label, vec![], 0)
            }
            "checkbox" => self.add_field(id, FieldKind::Check, value, String::new(), vec![], 0),
            "radio" => self.add_field(id, FieldKind::Radio, value, String::new(), vec![], 0),
            "password" => self.add_field(id, FieldKind::Password, value, String::new(), vec![], 0),
            "file" | "range" | "color" | "date" => {}
            _ => self.add_field(id, FieldKind::Text, value, String::new(), vec![], 0),
        }
    }

    fn button(&mut self, id: NodeId) {
        let label = self.dom.text_of(id);
        let label = if label.is_empty() { self.attr(id, "value").unwrap_or_default() } else { label };
        if label.is_empty() {
            return; // an icon-only button: nothing we could draw
        }
        let value = self.attr(id, "value").unwrap_or_default();
        self.add_field(id, FieldKind::Submit, value, fit(&self.m, &label, 120), vec![], 0);
    }

    fn collect_options(&self, id: NodeId, out: &mut Vec<(String, String)>, sel: &mut usize) {
        for &c in &self.dom.nodes[id].children {
            match self.dom.tag(c) {
                Some("option") => {
                    let label = self.dom.text_of(c);
                    let v = self.dom.attr(c, "value").map(String::from).unwrap_or_else(|| label.clone());
                    if self.dom.attr(c, "selected").is_some() {
                        *sel = out.len();
                    }
                    out.push((label, v));
                }
                Some("optgroup") => self.collect_options(c, out, sel),
                _ => {}
            }
        }
    }

    fn select(&mut self, id: NodeId) {
        let (mut options, mut sel) = (Vec::new(), 0);
        self.collect_options(id, &mut options, &mut sel);
        if options.is_empty() {
            return;
        }
        self.add_field(id, FieldKind::Select, String::new(), String::new(), options, sel);
    }

    // ---- tables

    fn rows(&self, t: NodeId) -> Vec<Vec<NodeId>> {
        let mut rows = Vec::new();
        let mut stack = vec![t];
        while let Some(n) = stack.pop() {
            let kids = &self.dom.nodes[n].children;
            for &c in kids.iter().rev() {
                match self.dom.tag(c) {
                    Some("tr") => {
                        let cells: Vec<NodeId> = self.dom.nodes[c].children.iter().copied().filter(|&k| matches!(self.dom.tag(k), Some("td" | "th"))).collect();
                        rows.push((c, cells));
                    }
                    Some("tbody" | "thead" | "tfoot") => stack.push(c),
                    _ => {}
                }
            }
        }
        // `stack` visits groups in reverse; sort rows by document order (node ids grow in order)
        rows.sort_by_key(|(id, _)| *id);
        rows.into_iter().map(|(_, c)| c).collect()
    }

    /// A row of nothing but blanks (spacer rows in old layouts): not drawn.
    fn row_blank(&self, cells: &[NodeId]) -> bool {
        fn media(d: &Dom, id: NodeId) -> bool {
            matches!(d.tag(id), Some("img" | "input" | "select" | "button" | "textarea" | "hr" | "table")) || d.nodes[id].children.iter().any(|&c| media(d, c))
        }
        cells.iter().all(|&c| self.dom.text_of(c).chars().all(|ch| ch.is_whitespace() || ch == '\u{3000}') && !media(self.dom, c))
    }

    fn longest_word(&self, id: NodeId) -> i32 {
        self.dom.text_of(id).split(' ').map(|w| self.m.width(w)).max().unwrap_or(0)
    }

    fn table(&mut self, t: NodeId) {
        self.block_edge(BLOCK_GAP);
        let rows = self.rows(t);
        let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if self.table_depth > 4 || ncols == 0 {
            self.walk_children(t);
            self.block_edge(BLOCK_GAP);
            return;
        }
        let avail = self.avail();
        let gap = 4;
        let mut nat = vec![0i32; ncols];
        let mut min = vec![0i32; ncols];
        for r in &rows {
            for (c, &cell) in r.iter().enumerate() {
                // a cell spanning several columns says nothing about the width of one of them
                if self.dom.attr(cell, "colspan").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1) > 1 {
                    continue;
                }
                let txt = self.dom.text_of(cell);
                nat[c] = nat[c].max(self.m.width(&txt) + 2);
                min[c] = min[c].max(self.longest_word(cell).min(80) + 2);
            }
        }
        let gaps = gap * (ncols as i32 - 1);
        let spans = rows.iter().flatten().any(|&c| self.dom.attr(c, "rowspan").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1) > 1);
        let widths: Option<Vec<i32>> = if spans {
            None
        } else if ncols == 1 {
            Some(vec![avail])
        } else if nat.iter().sum::<i32>() + gaps <= avail {
            Some(nat.clone())
        } else if min.iter().sum::<i32>() + gaps <= avail && ncols <= 6 {
            let extra = avail - gaps - min.iter().sum::<i32>();
            let want: i32 = nat.iter().zip(&min).map(|(n, m)| n - m).sum::<i32>().max(1);
            Some(nat.iter().zip(&min).map(|(n, m)| m + (n - m) * extra / want).collect())
        } else {
            None
        };
        self.table_depth += 1;
        let border = self.dom.attr(t, "border").map(|b| b != "0").unwrap_or(false);
        match widths {
            Some(ws) => {
                let (l0, r0) = (self.left, self.right);
                for r in &rows {
                    if self.row_blank(r) {
                        continue;
                    }
                    self.flush_line();
                    self.apply_gap();
                    let y0 = self.y;
                    let mut y_end = y0;
                    let mut x = l0;
                    for (c, &cell) in r.iter().enumerate() {
                        let span = self.dom.attr(cell, "colspan").and_then(|s| s.parse::<usize>().ok()).unwrap_or(1).clamp(1, ncols - c);
                        let w: i32 = ws[c..c + span].iter().sum::<i32>() + gap * (span as i32 - 1);
                        self.left = x;
                        self.right = x + w.max(self.m.wide);
                        self.y = y0;
                        self.gap = 0;
                        let st = self.style;
                        if self.dom.tag(cell) == Some("th") {
                            self.style.bold = true;
                        }
                        let ctr = self.center;
                        self.center = self.dom.attr(cell, "align") == Some("center");
                        self.walk_children(cell);
                        self.flush_line();
                        self.center = ctr;
                        self.style = st;
                        y_end = y_end.max(self.y);
                        x += w + gap;
                    }
                    self.left = l0;
                    self.right = r0;
                    self.y = y_end;
                    self.gap = if border { 0 } else { 1 };
                    if border {
                        self.apply_gap();
                        let (x, w, y) = (l0, avail, self.y);
                        self.mk_item(Item { x, y, w, h: 1, kind: ItemKind::Rule, link: None });
                        self.y += 3;
                    }
                    if self.full {
                        break;
                    }
                }
            }
            None => {
                // too wide for columns: stack every cell, rows separated by a gap
                for r in &rows {
                    if self.row_blank(r) {
                        continue;
                    }
                    for (c, &cell) in r.iter().enumerate() {
                        self.block_edge(if c == 0 { BLOCK_GAP } else { 1 });
                        let st = self.style;
                        if self.dom.tag(cell) == Some("th") {
                            self.style.bold = true;
                        }
                        self.walk_children(cell);
                        self.style = st;
                    }
                }
            }
        }
        self.table_depth -= 1;
        self.block_edge(BLOCK_GAP);
    }
}

/// Cut `s` with "..." so it is at most `w` wide.
pub fn fit(m: &Metrics, s: &str, w: i32) -> String {
    if m.width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        if m.width(&out) + m.char_w(ch) + 3 * m.latin > w {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn link_len(dom: &Dom, id: NodeId) -> i64 {
    if dom.tag(id) == Some("a") {
        return dom.text_of(id).chars().count() as i64;
    }
    dom.nodes[id].children.iter().map(|&c| link_len(dom, c)).sum()
}

/// The element holding the main text: the one whose paragraphs (long, not mostly links) add up to
/// the most characters; an `article` / `main` wins if it is nearly as good.
pub fn reader_root(dom: &Dom) -> Option<NodeId> {
    let n = dom.nodes.len();
    let mut score = vec![0i64; n];
    for id in 1..n {
        if dom.tag(id) != Some("p") {
            continue;
        }
        let len = dom.text_of(id).chars().count() as i64;
        if len < 30 || link_len(dom, id) * 2 > len {
            continue;
        }
        let parent = dom.nodes[id].parent;
        score[parent] += len;
        let gp = dom.nodes[parent].parent;
        if gp != parent {
            score[gp] += len / 2;
        }
    }
    let (best, best_score) = score.iter().enumerate().skip(1).max_by_key(|(_, s)| **s).map(|(i, s)| (i, *s))?;
    if best_score < 150 {
        return None;
    }
    let semantic = (1..n).filter(|&i| matches!(dom.tag(i), Some("article" | "main")) && score[i] * 2 >= best_score).max_by_key(|&i| score[i]);
    Some(semantic.unwrap_or(best))
}

/// Page furniture reader mode leaves out (navigation, forms, comments, share bars, ...).
fn reader_skips(dom: &Dom, id: NodeId, tag: &str) -> bool {
    if matches!(tag, "nav" | "aside" | "footer" | "header" | "form" | "button" | "select" | "textarea" | "input" | "figcaption" | "dialog") {
        return true;
    }
    let hay = format!("{} {}", dom.attr(id, "class").unwrap_or(""), dom.attr(id, "id").unwrap_or("")).to_ascii_lowercase();
    ["comment", "sidebar", "share", "social", "navbox", "menu", "footer", "promo", "related", "advert", "toc", "infobox", "hatnote", "mw-editsection"].iter().any(|k| hay.contains(k))
}

fn hidden(dom: &Dom, id: NodeId) -> bool {
    dom.attr(id, "hidden").is_some() || dom.attr(id, "style").map(|st| st.replace(' ', "").to_ascii_lowercase().contains("display:none")).unwrap_or(false)
}

/// Reader mode as plain text (for apps that draw it themselves): the paragraphs, headings, list
/// items and quotes of the main text block, white space folded; pictures, tables' furniture and
/// the reader-mode skips left out. None when the page has no article-like block.
pub fn reader_text(dom: &Dom) -> Option<Vec<String>> {
    Some(text_blocks(dom, reader_root(dom)?, true))
}

/// The text blocks under `root` (every block element and `<br>` ends one). `reader`: also leave
/// out what reader mode skips. Empty blocks are dropped.
pub fn text_blocks(dom: &Dom, root: NodeId, reader: bool) -> Vec<String> {
    fn walk(dom: &Dom, id: NodeId, reader: bool, cur: &mut String, out: &mut Vec<String>) {
        let tag = match &dom.nodes[id].kind {
            Kind::Text(t) => {
                cur.push_str(t);
                return;
            }
            Kind::Elem { tag, .. } => tag.as_str(),
        };
        if hidden(dom, id) || (reader && reader_skips(dom, id, tag)) || matches!(tag, "img" | "picture" | "figure" | "noscript") {
            return;
        }
        let block = is_block(tag) || tag == "br";
        if block {
            end_block(cur, out);
        }
        if tag == "li" {
            cur.push_str("* ");
        }
        for &c in &dom.nodes[id].children {
            walk(dom, c, reader, cur, out);
        }
        if block {
            end_block(cur, out);
        }
    }
    fn end_block(cur: &mut String, out: &mut Vec<String>) {
        let t = cur.split_whitespace().collect::<Vec<_>>().join(" ");
        cur.clear();
        if !t.is_empty() && t != "*" {
            out.push(t);
        }
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for &c in &dom.nodes[root].children {
        walk(dom, c, reader, &mut cur, &mut out);
    }
    end_block(&mut cur, &mut out);
    out
}

/// Wrap one paragraph to `max` px with the Browser's rules: Latin at spaces (a word longer than a
/// line is cut), Japanese between characters, never starting a line with a closing mark.
/// `width` measures a string as it will be drawn; the first line is `indent` px narrower.
pub fn wrap_text(m: &Metrics, text: &str, max: i32, indent: i32, width: &dyn Fn(&str) -> i32) -> Vec<String> {
    let text = fold(m, text);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut space = false;
    let room = |lines: &Vec<String>| if lines.is_empty() { max - indent } else { max };
    for tok in tokens(m, &text) {
        if tok == " " {
            space = !cur.is_empty();
            continue;
        }
        let joined = if space { format!("{cur} {tok}") } else { format!("{cur}{tok}") };
        space = false;
        if width(&joined) <= room(&lines) || cur.is_empty() && width(&tok) <= room(&lines) {
            cur = joined;
            continue;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        if width(&tok) <= room(&lines) {
            cur = tok;
            continue;
        }
        // Longer than a whole line: cut it.
        for ch in tok.chars() {
            cur.push(ch);
            if width(&cur) > room(&lines) && cur.chars().count() > 1 {
                cur.pop();
                lines.push(std::mem::take(&mut cur));
                cur.push(ch);
            }
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Lay a parsed document out `width` px wide. `base_url` is the page's own URL.
pub fn layout(dom: &Dom, base_url: &str, m: Metrics, width: i32) -> Page {
    layout_inner(dom, base_url, m, width, None)
}

/// Reader mode: only the main text block, with the page title; None if no article-like block exists.
pub fn layout_reader(dom: &Dom, base_url: &str, m: Metrics, width: i32) -> Option<Page> {
    let root = reader_root(dom)?;
    Some(layout_inner(dom, base_url, m, width, Some(root)))
}

fn layout_inner(dom: &Dom, base_url: &str, m: Metrics, width: i32, root: Option<NodeId>) -> Page {
    let mut base = base_url.to_string();
    if let Some(b) = dom.find("base") {
        if let Some(h) = dom.attr(b, "href") {
            if let Some(u) = url::resolve(base_url, h) {
                base = u;
            }
        }
    }
    let title = dom.find("title").map(|t| dom.text_of(t)).unwrap_or_default();
    let mut l = L {
        dom,
        m,
        base,
        p: Page { url: base_url.to_string(), title, ..Default::default() },
        y: 0,
        left: 0,
        right: width,
        gap: 0,
        line: vec![],
        line_w: 0,
        pending_space: false,
        center: false,
        pre: false,
        style: Style::default(),
        link: None,
        marker: None,
        form: None,
        lists: vec![],
        seen_link: vec![],
        table_depth: 0,
        full: false,
        reader: root.is_some(),
    };
    match root {
        Some(r) => {
            let dup = (1..dom.nodes.len()).any(|i| dom.tag(i) == Some("h1") && dom.text_of(i) == l.p.title);
            if !l.p.title.is_empty() && !dup {
                l.style.bold = true;
                let t = l.p.title.clone();
                l.add_text(&t);
                l.style = Style::default();
                l.block_edge(BLOCK_GAP + 2);
            }
            l.walk_children(r);
        }
        None => l.walk_children(0),
    }
    l.flush_line();
    l.p.reader = root.is_some();
    l.p.height = l.y;
    l.p.truncated = l.full || dom.truncated;
    // Items were emitted line by line, but table cells and bands interleave: sort by y (stable).
    l.p.items.sort_by_key(|i| i.y);
    l.p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::apps::browser::html;

    const M: Metrics = Metrics::fixed(6, 12);

    fn page(src: &str) -> Page {
        layout(&html::parse(src), "https://example.org/dir/page.html", M, 304)
    }

    fn lines(p: &Page) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut last_y = i32::MIN;
        for it in &p.items {
            if let ItemKind::Text { text, .. } = &it.kind {
                if it.y == last_y {
                    out.last_mut().unwrap().push_str(&format!("|{text}"));
                } else {
                    out.push(text.clone());
                }
                last_y = it.y;
            }
        }
        out
    }

    #[test]
    fn ascii_wraps_at_the_width() {
        let words = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau";
        let p = page(&format!("<p>{words}</p>"));
        let ls = lines(&p);
        assert!(ls.len() >= 3, "{ls:?}");
        for l in &ls {
            assert!(M.width(l) <= 304, "{l:?} is too wide");
            assert!(!l.ends_with(' ') && !l.starts_with(' '));
        }
        assert_eq!(ls.join(" "), words);
    }

    #[test]
    fn japanese_wraps_between_characters() {
        let jp = "吾輩は猫である。名前はまだ無い。どこで生れたかとんと見当がつかぬ。何でも薄暗いじめじめした所でニャーニャー泣いていた事だけは記憶している。";
        let p = page(&format!("<p>{jp}</p>"));
        let ls = lines(&p);
        assert!(ls.len() >= 2);
        for l in &ls {
            assert!(M.width(l) <= 304);
            // no line starts with a closing mark (kinsoku)
            assert!(!l.starts_with('。') && !l.starts_with('、'), "{l}");
        }
        assert_eq!(ls.concat(), jp);
        // 304 / 12 = 25 whole characters at most
        assert!(ls[0].chars().count() <= 25);
    }

    #[test]
    fn mixed_text_and_long_words() {
        let p = page(&format!("<p>日本語 mixed text and {} end</p>", "x".repeat(120)));
        for l in lines(&p) {
            assert!(M.width(&l) <= 304, "{l}");
        }
    }

    #[test]
    fn headings_paragraphs_lists() {
        let p = page("<h1>Title</h1><p>one</p><ul><li>a<li>b<ol><li>x</ol></ul><blockquote>q</blockquote><hr><pre>l1\n  l2</pre>");
        let ls = lines(&p);
        assert_eq!(ls[0], "Title");
        assert!(ls.iter().any(|l| l.starts_with("*|")), "{ls:?}");
        assert!(ls.iter().any(|l| l.starts_with("1.|")), "{ls:?}");
        assert!(ls.contains(&"  l2".to_string()), "{ls:?}");
        assert!(p.items.iter().any(|i| i.kind == ItemKind::Rule));
        assert!(p.items.iter().any(|i| i.kind == ItemKind::Bar));
        assert!(p.items.iter().any(|i| i.kind == ItemKind::Bg));
        assert!(p.height > 60);
        // y-sorted, positive heights
        assert!(p.items.windows(2).all(|w| w[0].y <= w[1].y));
    }

    #[test]
    fn unsupported_characters_are_folded() {
        let p = page("<p>a \u{2014} b \u{201C}q\u{201D} \u{2026} \u{1F600}</p>");
        assert_eq!(lines(&p), ["a - b \"q\" ... ?"]);
    }

    #[test]
    fn links_and_hit_testing() {
        let p = page("<p>go <a href='../x/y.html#f'>to the place</a> now <a href='/abs'>b</a></p>");
        assert_eq!(p.links, ["https://example.org/x/y.html#f", "https://example.org/abs"]);
        let it = p.items.iter().find(|i| i.link == Some(0)).unwrap();
        assert_eq!(p.hit(it.x + 2, it.y + 3), Some(Focus::Link(0)));
        assert_eq!(p.hit(it.x - 1, it.y + 3), None);
        let it2 = p.items.iter().find(|i| i.link == Some(1)).unwrap();
        assert_eq!(p.hit(it2.x, it2.y), Some(Focus::Link(1)));
        assert_eq!(p.focus.iter().map(|f| f.0).collect::<Vec<_>>(), [Focus::Link(0), Focus::Link(1)]);
        // a wrapped link yields several items with the same link
        let long = page(&format!("<a href='/w'>{}</a>", "word ".repeat(120)));
        assert!(long.items.iter().filter(|i| i.link == Some(0)).count() > 3);
        assert_eq!(long.focus.len(), 1);
    }

    #[test]
    fn unusable_hrefs_are_not_links() {
        let p = page("<a href='javascript:x()'>a</a><a href='mailto:me@x.org'>b</a><a>c</a>");
        assert!(p.links.is_empty());
    }

    #[test]
    fn tables_columns_or_stacked() {
        let p = page("<table><tr><th>Name<th>Value<tr><td>a<td>1</table>");
        let ls = lines(&p);
        assert!(ls[0].starts_with("Name|"), "{ls:?}");
        let wide = page(&format!("<table><tr>{}</tr></table>", "<td>some long cell text here</td>".repeat(8)));
        assert!(lines(&wide).len() >= 8, "stacked");
        let wrapping = page("<table><tr><td>the quick brown fox jumps over the lazy dog and keeps running far away from here<td>the quick brown fox jumps over the lazy dog and keeps running far</table>");
        for it in &wrapping.items {
            assert!(it.x + it.w <= 304 + 1, "{it:?}");
        }
    }

    #[test]
    fn content_after_a_table_returns_to_the_left_edge() {
        let p = page("<table border=1><tr><th>City<th>Temp<th>Sky</tr><tr><td>a<td>b<td>c</tr></table><h3>Links</h3><p>after</p>");
        for it in p.items.iter().filter(|i| matches!(&i.kind, ItemKind::Text { text, .. } if text == "Links" || text == "after")) {
            assert_eq!(it.x, 0);
        }
    }

    #[test]
    fn hidden_things_and_loose_controls_are_skipped() {
        let p = page("<p hidden>no</p><p style='display: none'>no</p><input type=checkbox><button>x</button><form><button></button><input name=q></form><p>yes</p>");
        assert_eq!(lines(&p).iter().filter(|l| l.as_str() == "no").count(), 0);
        assert_eq!(p.fields.len(), 1);
        assert!(lines(&p).contains(&"yes".to_string()));
    }

    #[test]
    fn reader_mode_keeps_the_article() {
        let body = "This is a long paragraph of real article text that goes on for a while. ";
        let src = format!("<title>Story</title><nav><a href=/a>Home</a><a href=/b>News</a></nav><div class=content><h1>Headline</h1><p>{body}{body}</p><p>{body}</p></div><div class=sidebar><p>{body}</p></div><footer>(c) 2026</footer>");
        let dom = html::parse(&src);
        let p = layout_reader(&dom, "https://x.org/", M, 304).unwrap();
        assert!(p.reader);
        let ls = lines(&p);
        assert_eq!(ls[0], "Story", "title first: {ls:?}");
        assert_eq!(ls.iter().filter(|l| l.contains("Headline")).count(), 1);
        assert!(ls.iter().any(|l| l.contains("Headline")));
        assert!(!ls.iter().any(|l| l.contains("(c) 2026") || l == "Home"));
        assert!(p.links.is_empty());
        // pages without prose have no reader view
        assert!(layout_reader(&html::parse("<ul><li><a href=x>link</a></ul><p>short</p>"), "https://x.org/", M, 304).is_none());
        // a link-only paragraph does not count as text
        let links = format!("<p><a href=x>{body}{body}</a></p>");
        assert!(layout_reader(&html::parse(&links), "https://x.org/", M, 304).is_none());
    }

    #[test]
    fn article_tag_wins_when_close() {
        let body = "Paragraph text that is long enough to count as article content here. ";
        let src = format!("<div><p>{body}{body}{body}</p></div><article><p>{body}{body}</p></article>");
        let dom = html::parse(&src);
        let r = reader_root(&dom).unwrap();
        assert_eq!(dom.tag(r), Some("article"));
    }

    #[test]
    fn append_keeps_links_fields_and_images_working() {
        let mut a = page("<p><a href=/a>one</a></p><form action=/s><input name=q value=x><input type=submit name=go value=Go></form>");
        let b = page("<p><a href=/b>two</a></p><img src=p.png width=40 height=30><form action=/t method=post><input name=z value=y></form>");
        let (ah, fields) = (a.height, a.fields.len());
        a.append(b, 8);
        assert_eq!(a.links, ["https://example.org/a", "https://example.org/b"]);
        assert!(a.height > ah);
        let two = a.items.iter().find(|i| i.link == Some(1)).unwrap();
        assert!(two.y >= ah + 8);
        assert_eq!(a.hit(two.x + 1, two.y + 1), Some(Focus::Link(1)));
        assert_eq!(a.fields.len(), fields + 1);
        assert_eq!(a.form_request(1, None).unwrap(), Request::Post { url: "https://example.org/t".into(), body: "z=y".into() });
        assert_eq!(a.form_request(0, Some(1)).unwrap(), Request::Get("https://example.org/s?q=x&go=Go".into()));
        assert!(a.items.iter().any(|i| i.kind == ItemKind::Img(0)));
        a.items.sort_by_key(|i| i.y);
        assert_eq!(a.focus.len(), 5);
    }

    #[test]
    fn blank_spacer_rows_are_dropped() {
        let p = page("<table><tr><td>\u{3000}<td>&nbsp;<tr><td>\u{3000}<td><a href=x>one</a><tr><td>\u{3000}<td>&nbsp;<tr><td>\u{3000}<td><a href=y>two</a></table>");
        assert!(p.height <= 2 * LINE_H + 6, "{}", p.height);
        assert_eq!(p.links.len(), 2);
    }

    #[test]
    fn a_colspan_cell_does_not_stretch_one_column() {
        let long = "word ".repeat(60);
        let p = page(&format!("<table><tr><td>a<td>b<tr><td colspan=2>{long}</table>"));
        let b = p.items.iter().find(|i| matches!(&i.kind, ItemKind::Text { text, .. } if text == "b")).unwrap();
        assert!(b.x < 40, "column 2 starts at x={}", b.x);
    }

    #[test]
    fn rowspan_tables_stack_instead_of_misaligning() {
        let p = page("<table><tr><td rowspan=2>left<td>a<td>b<tr><td>c<td>d</table>");
        assert_eq!(lines(&p), ["left", "a", "b", "c", "d"]);
    }

    #[test]
    fn forms_build_requests() {
        let p = page("<form action='/search' method=get><input name=q value='a b'><input type=hidden name=h value=1><input type=checkbox name=c value=y checked><input type=checkbox name=d><select name=s><option value=1>one<option value=2 selected>two</select><input type=submit name=go value=Go></form>");
        assert_eq!(p.forms.len(), 1);
        let req = p.form_request(0, Some(5)).unwrap();
        assert_eq!(req, Request::Get("https://example.org/search?q=a+b&h=1&c=y&s=2&go=Go".into()));
        assert_eq!(p.form_request(0, None).unwrap(), Request::Get("https://example.org/search?q=a+b&h=1&c=y&s=2".into()));
        let post = page("<form action=/p method=POST><input name=x value=1></form>");
        assert_eq!(post.form_request(0, None).unwrap(), Request::Post { url: "https://example.org/p".into(), body: "x=1".into() });
        assert!(p.focus.iter().any(|f| matches!(f.0, Focus::Field(_))));
    }

    #[test]
    fn radios_and_selects_toggle() {
        let mut p = page("<form><input type=radio name=r value=a checked><input type=radio name=r value=b><select name=s><option>x<option>y</select></form>");
        p.toggle(1);
        assert!(!p.fields[0].checked && p.fields[1].checked);
        p.toggle(2);
        assert_eq!(p.fields[2].sel, 1);
        p.toggle(2);
        assert_eq!(p.fields[2].sel, 0);
    }

    #[test]
    fn images_get_boxes_or_placeholders() {
        let p = page("<img src=a.png alt='Logo' width=100 height=50><img src=b.png><img src=t.gif width=1 height=1>");
        assert_eq!(p.images.len(), 2);
        assert_eq!((p.images[0].box_w, p.images[0].box_h), (100, 50));
        assert_eq!(p.images[1].box_h, LINE_H);
        assert_eq!(p.images[0].url, "https://example.org/dir/a.png");
        let big = page("<img src=a.png width=2000 height=1000>");
        assert!(big.images[0].box_w <= 304 && big.images[0].box_h <= 100);
    }

    #[test]
    fn a_refresh_inside_noscript_is_followed() {
        // DuckDuckGo's click-through page: JavaScript, and a refresh for browsers without it.
        let p = page("<html><head><meta name='referrer' content='origin'></head><body><script>window.parent.location.replace(\"https://rust-lang.org/\");</script><noscript><META http-equiv='refresh' content=\"0;URL=https://rust-lang.org/\"></noscript></body></html>");
        assert_eq!(p.refresh.as_deref(), Some("https://rust-lang.org/"));
    }

    #[test]
    fn anchors_title_base_refresh() {
        let p = page("<html><head><title> My  page </title><base href='https://other.net/x/'><meta http-equiv=refresh content='0; url=next.html'></head><body><h2 id=sec>S</h2><a href=y>y</a></body></html>");
        assert_eq!(p.title, "My page");
        assert_eq!(p.links[0], "https://other.net/x/y");
        assert_eq!(p.refresh.as_deref(), Some("https://other.net/x/next.html"));
        assert!(p.anchors.contains_key("sec"));
    }

    #[test]
    fn nasty_pages_do_not_panic_and_are_capped() {
        let nested = "<div>".repeat(5000) + "x";
        let p = page(&nested);
        assert!(p.height < MAX_HEIGHT + 100);
        let table = "<table>".repeat(50) + "<tr><td>x</table>".repeat(50).as_str();
        let _ = page(&table);
        let many = "<p>line</p>".repeat(20_000);
        let p = page(&many);
        assert!(p.items.len() <= MAX_ITEMS + 5);
        let _ = page("");
        let _ = page("<table></table><ul></ul><select></select><form><input></form>");
    }

    #[test]
    fn visible_range() {
        let p = page(&"<p>line</p>".repeat(100));
        let r = p.visible(500, 612);
        assert!(r.len() > 5 && r.len() < 40);
        for i in r {
            assert!(p.items[i].y < 612 && p.items[i].y + MAX_ITEM_H >= 500);
        }
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// Every word of the visible text must reach the layout (nothing may be swallowed).
    fn assert_no_content_lost(src: &str) -> Page {
        let dom = html::parse(src);
        let p = layout(&dom, "https://example.org/", M, 304);
        let body = dom.find("body").unwrap_or(0);
        let laid: String = p.items.iter().filter_map(|i| if let ItemKind::Text { text, .. } = &i.kind { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join(" ");
        for w in dom.text_of(body).split(' ').filter(|w| !w.is_empty()) {
            let w = fold(&M, w);
            assert!(laid.contains(&w), "word {w:?} is missing from the layout: {laid}");
        }
        p
    }

    #[test]
    fn the_real_example_com_page_shows_its_paragraph() {
        // The page really has only a <title> and one <p> (no h1, no link); the old one had both.
        let p = assert_no_content_lost(&fixture("example.org.html"));
        assert_eq!(p.title, "Example Domain");
        assert!(lines(&p)[0].starts_with("This domain is for use in documentation examples"));
        let old = assert_no_content_lost("<!doctype html><html><head><title>Example Domain</title><style>body{color:red}</style></head><body><div><h1>Example Domain</h1><p>This domain is for use in illustrative examples.</p><p><a href=\"https://iana.org/domains/example\">More information...</a></p></div></body></html>");
        assert_eq!(lines(&old)[0], "Example Domain");
        assert_eq!(old.links, ["https://iana.org/domains/example"]);
    }

    #[test]
    fn a_rich_article_loses_nothing() {
        let p = assert_no_content_lost(&fixture("rich.html"));
        let ls = lines(&p).join("\n");
        for want in ["Tides of the Northern Harbour", "J. Marlowe", "nested item", "2019|120 t", "Deeply nested", "code line 2", "Definition", "END-MARKER"] {
            assert!(ls.contains(want), "{want:?} missing:\n{ls}");
        }
        assert!(!ls.contains("not content"));
        assert_eq!(p.links.len(), 5, "{:?}", p.links);
        assert!(p.links.contains(&"https://iana.org/domains/example".to_string()));
        assert!(p.anchors.contains_key("two"));
        // every link is reachable with Tab
        assert_eq!(p.focus.len(), p.links.len());
    }

    #[test]
    fn reader_text_and_wrap_text_for_apps_that_draw_themselves() {
        let para = "This paragraph is long enough to count as the main text of the page, with more words, and then a few more words again so that it scores as an article on its own.";
        let dom = html::parse(&format!("<nav><p>{para} nav</p></nav><article><h1>Head</h1><img src=x><figure><p>caption</p></figure><p>{para}</p><p>Two<br>lines</p><ul><li>one</li></ul><div class=share><p>Share this</p></div></article>"));
        let t = reader_text(&dom).unwrap();
        assert_eq!(t, ["Head", para, "Two", "lines", "* one"]);
        assert!(reader_text(&html::parse("<p>short</p>")).is_none());
        // Wrapping: within the width, the first line narrower by the indent, Latin at spaces.
        let w = |s: &str| M.width(s);
        let ls = wrap_text(&M, para, 120, 12, &w);
        assert!(w(&ls[0]) <= 108 && ls.iter().all(|l| w(l) <= 120), "{ls:?}");
        assert_eq!(ls.join(" "), para);
        // Japanese between characters, never starting a line with a closing mark.
        let jp = "吾輩は猫である。名前はまだ無い。どこで生れたかとんと見当がつかぬ。";
        for width in [60, 72, 96, 108] {
            let ls = wrap_text(&M, jp, width, 0, &w);
            assert_eq!(ls.concat(), jp);
            assert!(ls.iter().all(|l| w(l) <= width && !l.starts_with('。') && !l.starts_with('、')), "{width}: {ls:?}");
        }
        // A word longer than the line is cut.
        let ls = wrap_text(&M, &"x".repeat(50), 60, 0, &w);
        assert!(ls.iter().all(|l| w(l) <= 60) && ls.concat().len() == 50);
    }
}
