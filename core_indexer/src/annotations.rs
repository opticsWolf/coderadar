//! Annotation + comment concepts (P6a, DR-44).
//!
//! The fourth and fifth non-code concept kinds: `annotation` (a markdown
//! list — bullets, numbered, and/or checkable task lists, nothing else)
//! and `comment` (freeform markdown). Both ride the ledger the way code
//! does — create mints `annotation:{about}:{n}`, an edit supersedes the
//! same id, retire soft-deletes it — so "who checked what when" is the
//! row's own `history`, the same way `history_of` gets file history.
//!
//! The write boundary lives here (INV-14): one linter, two gates. Gate 1
//! (both kinds) repairs deterministic defects and stores the repaired
//! canonical form; Gate 2 (annotations only) refuses anything that is not
//! a flat list after repair, citing the offending line. Commit messages
//! pass through neither gate (frozen git history, §5 of the proposal).
//!
//! Reads are ledger-direct via the diagnostic connection (the `history_of`
//! pattern): neither kind joins the projection, so search, stats,
//! embeddings, and the coverage universes cannot see them (INV-13 — the
//! exclusions live in `live_concept_ids` / `all_concept_ids`).

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Concept kind for enforced-list markdown rows.
pub const ANNOTATION_KIND: &str = "annotation";
/// Concept kind for freeform markdown rows.
pub const COMMENT_KIND: &str = "comment";
/// Payload schema version, matching the commit concept's.
pub const META_VERSION: u32 = 2;

/// Parser options: tasklists (the checkbox grammar annotations enforce)
/// plus tables (comment fidelity — a table must round-trip as a table,
/// not decay into paragraph text). Everything else stays off so stored
/// bytes never gain smart punctuation, footnotes, or math the author did
/// not write.
fn parser_options() -> Options {
    Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES
}

/// A linted body: the canonical stored form plus its unchecked-item count
/// (`open` is meaningful for annotations, always 0 for comments).
#[derive(Debug)]
pub struct Linted {
    pub body: String,
    pub open: usize,
    pub repaired: bool,
}

/// What Gate 1 / Gate 2 refused, with the offending span cited.
#[derive(Debug, Clone, PartialEq)]
pub enum LintFault {
    /// An annotation block that is not a list after repair (Gate 2).
    /// `line` is 1-based into the repaired input.
    NotAList { line: usize, snippet: String },
    /// A list nested inside a list item (v1 is flat).
    NestedList { line: usize, snippet: String },
    /// Nothing storable survived repair (empty input).
    Empty,
}

impl std::fmt::Display for LintFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LintFault::NotAList { line, snippet } => write!(
                f,
                "annotation body must be a markdown list (bullets, numbered, \
                 and/or checkable items) — line {line} is not a list item: {snippet:?}"
            ),
            LintFault::NestedList { line, snippet } => write!(
                f,
                "annotation lists are flat in v1 (nesting deferred) — \
                 line {line} nests a list: {snippet:?}"
            ),
            LintFault::Empty => write!(f, "nothing storable: the body is empty"),
        }
    }
}

/// Parse `raw` markdown, repair deterministic defects, and return the
/// canonical stored form (Gate 1). `kind` selects Gate 2 (annotations).
///
/// Repair-then-store is the generous direction: trailing whitespace, tabs
/// outside fenced code, unclosed fences, list-marker spacing, and missing
/// blank lines before fenced blocks, ATX headings, and ordered lists after
/// text all have one deterministic fix, and the fixed form is what every
/// read sees. Refusal is the last resort: only the list grammar (Gate 2)
/// or an empty body refuses, because the parser itself is total — there
/// is no input it cannot read, only shapes annotations cannot store.
pub fn lint(kind: &str, raw: &str) -> Result<Linted, LintFault> {
    let (repaired_text, repaired) = repair(raw);
    if repaired_text.trim().is_empty() {
        return Err(LintFault::Empty);
    }
    let events: Vec<(Event<'_>, std::ops::Range<usize>)> =
        Parser::new_ext(&repaired_text, parser_options())
            .into_offset_iter()
            .collect();
    let analysis = analyze(&events, &repaired_text);
    if kind == ANNOTATION_KIND {
        if let Some(offense) = analysis.offense {
            return Err(offense);
        }
    }
    Ok(Linted {
        body: render_canonical(&events),
        open: if kind == ANNOTATION_KIND {
            analysis.unchecked
        } else {
            0
        },
        repaired,
    })
}

struct Analysis {
    offense: Option<LintFault>,
    unchecked: usize,
}

/// One pass over the parsed events: Gate 2 grammar (annotations) plus the
/// unchecked task-item count. `source` is the repaired input the offsets
/// point into.
fn analyze(events: &[(Event<'_>, std::ops::Range<usize>)], source: &str) -> Analysis {
    let mut depth: usize = 0;
    let mut item_depth: usize = 0;
    let mut offense: Option<LintFault> = None;
    let mut unchecked = 0usize;
    for (event, span) in events {
        match event {
            Event::Start(tag) => {
                let is_block = matches!(
                    tag,
                    Tag::Paragraph
                        | Tag::Heading { .. }
                        | Tag::CodeBlock(_)
                        | Tag::List(_)
                        | Tag::Item
                        | Tag::BlockQuote(_)
                        | Tag::HtmlBlock
                        | Tag::Table(_)
                        | Tag::TableHead
                        | Tag::FootnoteDefinition(_)
                );
                if depth == 0 && is_block && !matches!(tag, Tag::List(_)) && offense.is_none() {
                    offense = Some(LintFault::NotAList {
                        line: line_of(source, span.start),
                        snippet: snippet_of(source, span.start),
                    });
                }
                // Only a List inside an Item nests: a quoted list sits
                // under its BlockQuote, not under an item.
                if item_depth > 0 && matches!(tag, Tag::List(_)) && offense.is_none() {
                    offense = Some(LintFault::NestedList {
                        line: line_of(source, span.start),
                        snippet: snippet_of(source, span.start),
                    });
                }
                if is_block {
                    depth += 1;
                }
                if matches!(tag, Tag::Item) {
                    item_depth += 1;
                }
            }
            Event::End(end) => {
                if matches!(end, TagEnd::Item) {
                    item_depth = item_depth.saturating_sub(1);
                }
                // Block tags balance their Starts exactly; inline ends
                // (Emphasis, Link, …) never touched `depth`.
                if matches!(
                    end,
                    TagEnd::Paragraph
                        | TagEnd::Heading(_)
                        | TagEnd::CodeBlock
                        | TagEnd::List(_)
                        | TagEnd::Item
                        | TagEnd::BlockQuote(_)
                        | TagEnd::HtmlBlock
                        | TagEnd::Table
                        | TagEnd::TableHead
                        | TagEnd::FootnoteDefinition
                ) {
                    depth = depth.saturating_sub(1);
                }
            }
            Event::TaskListMarker(checked) => {
                unchecked += usize::from(!checked);
            }
            _ => {}
        }
    }
    Analysis { offense, unchecked }
}

fn line_of(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())].matches('\n').count() + 1
}

fn snippet_of(source: &str, offset: usize) -> String {
    let start = source[..offset.min(source.len())]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let end = source[start..]
        .find('\n')
        .map(|i| start + i)
        .unwrap_or(source.len());
    let line: String = source[start..end].chars().take(80).collect();
    line.trim().to_string()
}

// ── Gate 1: line repairs ────────────────────────────────────────────────

/// Gate 1 repairs on lines. Returns the repaired text plus whether any
/// repair fired. Fenced code blocks are opaque to every rule except
/// trailing-whitespace stripping and fence closure.
fn repair(raw: &str) -> (String, bool) {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut fired = normalized != raw;
    // A terminal newline terminates the last line; it is not a blank
    // line of its own (without this, closing an unclosed fence appends
    // after a phantom blank line that was never authored).
    let stripped = normalized.strip_suffix('\n').unwrap_or(&normalized);
    let mut out: Vec<String> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in stripped.split('\n') {
        if let Some((ch, run)) = fence {
            let trimmed = line.trim_end();
            if trimmed != line {
                fired = true;
            }
            if is_fence_close(trimmed, ch, run) {
                fence = None;
            }
            out.push(trimmed.to_string());
            continue;
        }
        if let Some((ch, run)) = fence_open(line) {
            let trimmed = line.trim_end();
            if trimmed != line {
                fired = true;
            }
            fence = Some((ch, run));
            out.push(trimmed.to_string());
            continue;
        }
        let mut text = line.trim_end().to_string();
        if text != line {
            fired = true;
        }
        if text.contains('\t') {
            text = expand_tabs(&text);
            fired = true;
        }
        let marked = canonical_marker(&text);
        if marked != text {
            fired = true;
            text = marked;
        }
        out.push(text);
    }
    if let Some((ch, run)) = fence {
        // Unclosed fence: append the matching closer rather than refuse.
        out.push(ch.to_string().repeat(run));
        fired = true;
    }
    // Blank-line insertion: fenced blocks, ATX headings, and ordered
    // lists after running text each parse differently without one, so a
    // missing separator has exactly one deterministic fix. The pass
    // tracks fence state so an appended closer never earns a separator
    // of its own (it is not an opener).
    let mut final_lines: Vec<String> = Vec::with_capacity(out.len());
    let mut open_fence: Option<(char, usize)> = None;
    for line in &out {
        if let Some((ch, run)) = open_fence {
            if is_fence_close(line.trim_end(), ch, run) {
                open_fence = None;
            }
            final_lines.push(line.clone());
            continue;
        }
        if let Some((ch, run)) = fence_open(line) {
            open_fence = Some((ch, run));
            let prev_blank = final_lines
                .last()
                .is_none_or(|p: &String| p.trim().is_empty());
            if !prev_blank && !final_lines.is_empty() {
                final_lines.push(String::new());
                fired = true;
            }
            final_lines.push(line.clone());
            continue;
        }
        let prev_blank = final_lines
            .last()
            .is_none_or(|p: &String| p.trim().is_empty());
        let needs_sep = !prev_blank
            && !final_lines.is_empty()
            && (is_atx_heading(line)
                || (is_ordered_item(line) && !is_list_item(final_lines.last().unwrap())));
        if needs_sep {
            final_lines.push(String::new());
            fired = true;
        }
        final_lines.push(line.clone());
    }
    (final_lines.join("\n"), fired)
}

fn is_fence_close(line: &str, ch: char, run: usize) -> bool {
    let stripped = line.trim_start();
    if !stripped.starts_with(ch) {
        return false;
    }
    let count = stripped.chars().take_while(|&c| c == ch).count();
    count >= run && stripped[count..].trim().is_empty()
}

fn fence_open(line: &str) -> Option<(char, usize)> {
    let spaces = line.chars().take_while(|&c| c == ' ').count();
    if spaces > 3 {
        return None;
    }
    let stripped = &line[spaces..];
    for ch in ['`', '~'] {
        if stripped.starts_with(ch) {
            let run = stripped.chars().take_while(|&c| c == ch).count();
            if run >= 3 {
                return Some((ch, run));
            }
        }
    }
    None
}

fn is_atx_heading(line: &str) -> bool {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|&c| c == '#').count();
    (1..=6).contains(&hashes) && t[hashes..].starts_with(' ')
}

fn is_list_item(line: &str) -> bool {
    is_bullet_item(line) || is_ordered_item(line)
}

fn is_bullet_item(line: &str) -> bool {
    if !indent_ok(line) {
        return false;
    }
    let t = line.trim_start();
    t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ")
}

fn is_ordered_item(line: &str) -> bool {
    if !indent_ok(line) {
        return false;
    }
    let t = line.trim_start();
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && matches!(t[digits..].chars().next(), Some('.' | ')'))
}

fn indent_ok(line: &str) -> bool {
    line.chars().take_while(|&c| c == ' ').count() <= 3
}

fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for c in line.chars() {
        if c == '\t' {
            let spaces = 4 - (col % 4);
            out.push_str(&" ".repeat(spaces));
            col += spaces;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// Canonicalize a list-item marker: `*`/`+` become `-`, `)` becomes `.`,
/// and a missing space after the marker is inserted. Anything else
/// passes through untouched.
fn canonical_marker(line: &str) -> String {
    let indent_len = line.chars().take_while(|&c| c == ' ').count();
    if indent_len > 3 {
        return line.to_string();
    }
    let (indent, rest) = line.split_at(indent_len);
    let mut chars = rest.chars();
    match chars.next() {
        Some(m) if m == '*' || m == '+' || m == '-' => {
            let tail: String = chars.collect();
            let spaced = if tail.starts_with(' ') {
                tail
            } else {
                format!(" {tail}")
            };
            format!("{indent}-{spaced}")
        }
        Some(_) if rest.starts_with(|c: char| c.is_ascii_digit()) => {
            // `rest` starts with the digit just consumed: find the run
            // end, then demand the delimiter. (A `take_while` chain
            // would swallow the delimiter — the failing element is
            // consumed, not peeked.)
            let num_end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            let (digits, tail) = rest.split_at(num_end);
            match tail.strip_prefix(['.', ')']) {
                Some(body) => {
                    let body = body.strip_prefix(' ').unwrap_or(body);
                    format!("{indent}{digits}. {body}")
                }
                None => line.to_string(),
            }
        }
        _ => line.to_string(),
    }
}

// ── Canonical rendering ───────────────────────────────────────────────

/// Render parsed events back to the one canonical markdown shape.
/// Deterministic by construction: the same events always render the same
/// bytes, and rendering is a fixpoint (linting stored output returns it
/// byte-identical — pinned by `canonical_is_a_fixpoint`).
fn render_canonical(events: &[(Event<'_>, std::ops::Range<usize>)]) -> String {
    let mut r = Renderer::new();
    for (event, _) in events {
        r.event(event);
    }
    r.finish()
}

struct TableState {
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: Option<String>,
    header_done: bool,
}

struct Renderer {
    out: String,
    /// Pending list-item marker (set at Item start, consumed by content).
    item_marker: Option<String>,
    ordered_next: Vec<Option<u64>>,
    /// Destinations for open links/images (Start pushed, End pops).
    link_stack: Vec<(String, Option<String>)>,
    pending_heading: Option<u8>,
    blockquote_depth: usize,
    in_item: bool,
    in_code: bool,
    table: Option<TableState>,
    line_start: bool,
}

impl Renderer {
    fn new() -> Self {
        Renderer {
            out: String::new(),
            item_marker: None,
            ordered_next: Vec::new(),
            link_stack: Vec::new(),
            pending_heading: None,
            blockquote_depth: 0,
            in_item: false,
            in_code: false,
            table: None,
            line_start: true,
        }
    }

    /// Push raw text, splitting embedded newlines into fresh lines so
    /// `line_start` stays honest (code literals carry their own `\n`).
    /// `escape` applies line-start escaping (off inside code and cells).
    fn push(&mut self, s: &str, escape: bool) {
        let mut first = true;
        for chunk in s.split('\n') {
            if !first {
                self.newline();
            }
            first = false;
            if chunk.is_empty() {
                continue;
            }
            if self.line_start {
                if let Some(table) = self.table.as_mut() {
                    if let Some(cell) = table.current_cell.as_mut() {
                        if !cell.is_empty() {
                            cell.push(' ');
                        }
                        cell.push_str(&chunk.replace('|', "\\|"));
                        continue;
                    }
                }
                for _ in 0..self.blockquote_depth {
                    self.out.push_str("> ");
                }
                if let Some(marker) = self.item_marker.take() {
                    self.out.push_str(&marker);
                }
            }
            if escape && self.line_start && needs_escape(chunk) {
                self.out.push('\\');
            }
            self.out.push_str(chunk);
            self.line_start = false;
        }
    }

    fn text(&mut self, s: &str) {
        let escape = !self.in_code;
        self.push(s, escape);
    }

    fn newline(&mut self) {
        self.out.push('\n');
        self.line_start = true;
    }

    fn fresh_line(&mut self) {
        if !self.line_start {
            self.newline();
        }
    }

    fn blank_sep(&mut self) {
        if self.out.is_empty() {
            return;
        }
        while self.out.ends_with('\n') {
            self.out.pop();
        }
        self.out.push_str("\n\n");
        self.line_start = true;
    }

    fn event(&mut self, event: &Event<'_>) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    if !self.in_item {
                        self.blank_sep();
                    }
                }
                Tag::Heading { level, .. } => {
                    self.blank_sep();
                    self.pending_heading = Some(*level as u8);
                }
                Tag::CodeBlock(kind) => {
                    self.blank_sep();
                    let info = match kind {
                        CodeBlockKind::Fenced(info) => info.trim().to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    // A literal containing a backtick run needs a longer
                    // fence; the run length is unknowable before the
                    // literal arrives, so emit the opener and fix it up
                    // at close (see End(CodeBlock)).
                    self.push(&format!("```{info}"), false);
                    let _ = info;
                    self.newline();
                    self.in_code = true;
                }
                Tag::List(start) => {
                    if !self.in_item {
                        self.blank_sep();
                    } else {
                        self.fresh_line();
                    }
                    self.ordered_next.push(*start);
                }
                Tag::Item => {
                    self.fresh_line();
                    self.in_item = true;
                    let n = self.ordered_next.last().copied().flatten();
                    self.item_marker = Some(match n {
                        Some(next) => {
                            let m = format!("{next}. ");
                            if let Some(slot) = self.ordered_next.last_mut() {
                                *slot = Some(next + 1);
                            }
                            m
                        }
                        None => "- ".to_string(),
                    });
                }
                Tag::BlockQuote(_) => {
                    self.blank_sep();
                    self.blockquote_depth += 1;
                }
                Tag::Emphasis => self.text("*"),
                Tag::Strong => self.text("**"),
                Tag::Strikethrough => self.text("~~"),
                Tag::Link {
                    dest_url, title, ..
                } => {
                    self.link_stack.push((
                        dest_url.to_string(),
                        if title.is_empty() {
                            None
                        } else {
                            Some(title.to_string())
                        },
                    ));
                    self.text("[");
                }
                Tag::Image {
                    dest_url, title, ..
                } => {
                    self.link_stack.push((
                        dest_url.to_string(),
                        if title.is_empty() {
                            None
                        } else {
                            Some(title.to_string())
                        },
                    ));
                    self.text("![");
                }
                Tag::Table(_) => {
                    self.blank_sep();
                    self.table = Some(TableState {
                        rows: Vec::new(),
                        current_row: Vec::new(),
                        current_cell: None,
                        header_done: false,
                    });
                }
                Tag::TableHead | Tag::TableRow => {
                    if let Some(table) = self.table.as_mut() {
                        table.current_row = Vec::new();
                    }
                }
                Tag::TableCell => {
                    if let Some(table) = self.table.as_mut() {
                        table.current_cell = Some(String::new());
                    }
                }
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph => {
                    self.fresh_line();
                }
                TagEnd::Heading(_) => {
                    self.pending_heading = None;
                    self.fresh_line();
                }
                TagEnd::CodeBlock => {
                    self.in_code = false;
                    self.fresh_line();
                    // Widen the fence when the literal outgrew it: the
                    // opener sits at the last ``` line before this close.
                    self.widen_fence();
                    self.push("```", false);
                    self.newline();
                }
                TagEnd::List(_) => {
                    self.ordered_next.pop();
                    self.fresh_line();
                }
                TagEnd::Item => {
                    self.fresh_line();
                    self.in_item = false;
                }
                TagEnd::BlockQuote(_) => {
                    self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                    self.fresh_line();
                }
                TagEnd::Emphasis => self.text("*"),
                TagEnd::Strong => self.text("**"),
                TagEnd::Strikethrough => self.text("~~"),
                TagEnd::Link => {
                    self.text("]");
                    if let Some((dest, title)) = self.link_stack.pop() {
                        match title {
                            Some(t) => self.push(&format!("({dest} \"{t}\")"), false),
                            None => self.push(&format!("({dest})"), false),
                        }
                    }
                }
                TagEnd::Image => {
                    self.text("]");
                    if let Some((dest, title)) = self.link_stack.pop() {
                        match title {
                            Some(t) => self.push(&format!("({dest} \"{t}\")"), false),
                            None => self.push(&format!("({dest})"), false),
                        }
                    }
                }
                TagEnd::Table => {
                    self.emit_table();
                    self.table = None;
                    self.fresh_line();
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    if let Some(table) = self.table.as_mut() {
                        if !table.current_row.is_empty() || !table.header_done {
                            table.rows.push(std::mem::take(&mut table.current_row));
                        }
                        if matches!(end, TagEnd::TableHead) {
                            table.header_done = true;
                        }
                    }
                }
                TagEnd::TableCell => {
                    if let Some(table) = self.table.as_mut() {
                        if let Some(cell) = table.current_cell.take() {
                            table.current_row.push(cell);
                        }
                    }
                }
                _ => {}
            },
            Event::Text(text) => {
                if let Some(level) = self.pending_heading.take() {
                    self.push(&format!("{} ", "#".repeat(level as usize)), false);
                }
                self.text(text);
            }
            Event::Code(code) => {
                self.text("`");
                self.text(&code.replace('`', "\\`"));
                self.text("`");
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                self.text(html);
            }
            Event::SoftBreak => {
                self.newline();
            }
            Event::HardBreak => {
                self.text("\\");
                self.newline();
            }
            Event::Rule => {
                self.blank_sep();
                self.text("---");
                self.newline();
            }
            Event::TaskListMarker(checked) => {
                // The marker refines the pending item bullet the Item
                // start left behind: `- ` becomes `- [ ] ` / `- [x] `.
                // Ordered markers never occur (task items are bullets).
                self.item_marker = self.item_marker.take().map(|_| {
                    if *checked {
                        "- [x] ".to_string()
                    } else {
                        "- [ ] ".to_string()
                    }
                });
            }
            _ => {}
        }
    }

    /// Emit buffered table rows in GFM shape (header, delimiter, body).
    /// A missing head still renders: the first row doubles as header.
    fn emit_table(&mut self) {
        let rows: Vec<Vec<String>> = match self.table.as_mut() {
            Some(table) => std::mem::take(&mut table.rows),
            None => return,
        };
        if rows.is_empty() {
            return;
        }
        let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        for (i, row) in rows.iter().enumerate() {
            let mut cells = row.clone();
            cells.resize(width, String::new());
            self.push(&format!("| {} |", cells.join(" | ")), false);
            self.newline();
            if i == 0 {
                self.push(&format!("|{}|", vec![" --- "; width].join("|")), false);
                self.newline();
            }
        }
    }

    /// Lengthen the most recent fence opener to outgrow the literal's
    /// longest backtick run (canonical fences always fit).
    fn widen_fence(&mut self) {
        let Some(open_pos) = self.out.rfind("```") else {
            return;
        };
        let after = &self.out[open_pos..];
        let opener_run = after.chars().take_while(|&c| c == '`').count();
        let literal = &after[opener_run..];
        let mut longest = 0usize;
        let mut run = 0usize;
        for c in literal.chars() {
            if c == '`' {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        if longest >= opener_run {
            let wider = "`".repeat(longest + 1);
            self.out
                .replace_range(open_pos..open_pos + opener_run, &wider);
        }
    }

    fn finish(mut self) -> String {
        while self.out.ends_with('\n') {
            self.out.pop();
        }
        self.out.push('\n');
        self.out
    }
}

fn needs_escape(chunk: &str) -> bool {
    if chunk.starts_with('\\') || chunk.starts_with('#') || chunk.starts_with('>') {
        return true;
    }
    if chunk.starts_with("- ") || chunk.starts_with("* ") || chunk.starts_with("+ ") {
        return true;
    }
    let digits = chunk.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let after = &chunk[digits..];
        if after.starts_with(". ") || after.starts_with(") ") {
            return true;
        }
    }
    // `[text]` at line start would re-parse as a shortcut reference link
    // (or nothing, dropping the brackets on some renderers); the
    // backslash keeps stored bytes literal.
    if chunk.starts_with('[') {
        return true;
    }
    false
}

// ── Store operations ────────────────────────────────────────────────────

/// What the write boundary refused, distinct from lint faults.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnError {
    /// `about` is empty, carries a newline, or names an annotation/comment
    /// (nesting is flat-deferred in v1).
    InvalidAbout(String),
    /// The id is not an `{annotation,comment}:{about}:{n}` shape.
    InvalidId(String),
    /// No live-or-retired row holds this id.
    NotFound(String),
    /// The id names a live-or-retired row of another kind. This is the
    /// guard that keeps user writes off code concepts: only annotation
    /// and comment rows are revisable or retirable.
    WrongKind { id: String, kind: String },
    /// Revising a retired row (retire-first: history stays readable, the
    /// row itself is closed — create a new annotation instead).
    AlreadyRetired(String),
    /// A lint fault (Gate 1 empty / Gate 2 grammar).
    Lint(LintFault),
    /// The ledger refused (I/O, schema, trigger).
    Store(String),
}

impl std::fmt::Display for AnnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnnError::InvalidAbout(a) => write!(
                f,
                "cannot annotate {a:?}: about must be a non-empty concept id, \
                 never another annotation or comment (v1 is flat)"
            ),
            AnnError::InvalidId(id) => write!(
                f,
                "not an annotation or comment id ({id:?}): \
                 expected annotation:{{about}}:{{n}} or comment:{{about}}:{{n}}"
            ),
            AnnError::NotFound(id) => write!(f, "no annotation or comment row {id:?}"),
            AnnError::WrongKind { id, kind } => write!(
                f,
                "{id:?} is a {kind} concept, not an annotation or comment: \
                 user writes never touch code rows"
            ),
            AnnError::AlreadyRetired(id) => write!(
                f,
                "{id:?} is retired: annotate anew rather than revising a closed row"
            ),
            AnnError::Lint(fault) => write!(f, "{fault}"),
            AnnError::Store(e) => write!(f, "store error: {e}"),
        }
    }
}

impl From<LintFault> for AnnError {
    fn from(f: LintFault) -> Self {
        AnnError::Lint(f)
    }
}

/// The stored content JSON shared by both kinds.
fn content_json(kind: &str, about: &str, n: u64, body: &str, open: usize) -> String {
    serde_json::json!({
        "meta_version": META_VERSION,
        "kind": kind,
        "about": about,
        "n": n,
        "body": body,
        "open": open,
    })
    .to_string()
}

fn extra_json(kind: &str, about: &str) -> String {
    serde_json::json!({ "coderadar": { "kind": kind, "about": about } }).to_string()
}

/// Split `annotation:{about}:{n}` / `comment:{about}:{n}` into
/// `(kind, about, n)`. The about half may itself contain colons
/// (`branch:main:content:a.py`, `commit:{sha}`), so the split is on the
/// LAST colon and the tail must be numeric.
pub fn split_id(id: &str) -> Option<(&str, &str, u64)> {
    let (kind, rest) = id
        .strip_prefix("annotation:")
        .map(|rest| (ANNOTATION_KIND, rest))
        .or_else(|| id.strip_prefix("comment:").map(|rest| (COMMENT_KIND, rest)))?;
    let (about, tail) = rest.rsplit_once(':')?;
    if about.is_empty() {
        return None;
    }
    Some((kind, about, tail.parse::<u64>().ok()?))
}

fn validate_about(about: &str) -> Result<(), AnnError> {
    if about.trim().is_empty() || about.contains('\n') {
        return Err(AnnError::InvalidAbout(about.to_string()));
    }
    if about.starts_with("annotation:") || about.starts_with("comment:") {
        return Err(AnnError::InvalidAbout(about.to_string()));
    }
    Ok(())
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Next sequence number for `(kind, about)`: one past the largest `n`
/// ever minted — current rows AND retired ones (the log scan), so a
/// retire-then-re-annotate never reuses an id. Starts at 1.
pub fn next_sequence(
    store: &crate::storage::CodeGraphStore,
    kind: &str,
    about: &str,
) -> Result<u64, AnnError> {
    let prefix = format!("{kind}:{about}:");
    let pattern = format!("{}%", escape_like(&prefix));
    let (_guard, conn) = store
        .open_diagnostic_conn()
        .map_err(|e| AnnError::Store(format!("{e:?}")))?;
    let ids: Vec<String> = crate::storage::runtime()
        .block_on(async {
            let mut out = Vec::new();
            let mut rows = conn
                .query(
                    "SELECT id FROM concepts WHERE id LIKE ?1 ESCAPE '\\'",
                    libsql::params![pattern.clone()],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                out.push(row.get::<String>(0).unwrap_or_default());
            }
            let mut log_rows = conn
                .query(
                    "SELECT DISTINCT entity_id FROM transaction_log \
                     WHERE table_name = 'concepts' AND entity_id LIKE ?1 ESCAPE '\\'",
                    libsql::params![pattern],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            while let Some(row) = log_rows.next().await.map_err(macrame::DbError::Engine)? {
                out.push(row.get::<String>(0).unwrap_or_default());
            }
            Ok::<_, macrame::DbError>(out)
        })
        .map_err(|e| AnnError::Store(format!("{e:?}")))?;
    let mut max_n = 0u64;
    for id in &ids {
        // The LIKE arm can over-match longer abouts sharing the prefix
        // (`annotation:x:%` also matches `annotation:x:y:1`): only
        // exact-about ids count.
        if id.starts_with(&prefix) {
            if let Some((_, _, n)) = split_id(id) {
                max_n = max_n.max(n);
            }
        }
    }
    Ok(max_n + 1)
}

/// What `create` minted.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Created {
    pub id: String,
    pub about: String,
    pub n: u64,
    pub body: String,
    pub open: usize,
    pub repaired: bool,
}

/// Mint `annotation:{about}:{n}` / `comment:{about}:{n}` with a linted
/// body. Stamps are present-tense by construction — the caller passes no
/// mode, and the row takes `now` on the trunk (INV-14's clock rule, §7 of
/// the proposal). No existence gate on `about`: annotations must outlive
/// their target (retire-first rationale), and existence is
/// branch-relative while rows are trunk-global — a write-time check
/// would refuse rows the reader's branch can still name.
pub fn create(
    store: &crate::storage::CodeGraphStore,
    kind: &str,
    about: &str,
    raw_body: &str,
) -> Result<Created, AnnError> {
    if kind != ANNOTATION_KIND && kind != COMMENT_KIND {
        return Err(AnnError::Store(format!("unknown annotation kind {kind:?}")));
    }
    validate_about(about)?;
    let linted = lint(kind, raw_body)?;
    let n = next_sequence(store, kind, about)?;
    let id = format!("{kind}:{about}:{n}");
    let upsert = macrame::ConceptUpsert::new(id.clone(), id.clone())
        .content(content_json(kind, about, n, &linted.body, linted.open))
        .extra(extra_json(kind, about))
        .valid_from(crate::storage::now_iso8601())
        .valid_to(crate::storage::TS_OPEN.to_string())
        .retired(false);
    let written = store
        .upsert_concepts_bulk(std::slice::from_ref(&upsert))
        .map_err(|e| AnnError::Store(format!("{e:?}")))?;
    if written != 1 {
        return Err(AnnError::Store(format!(
            "create wrote {written} concepts for {id:?}, expected 1"
        )));
    }
    Ok(Created {
        id,
        about: about.to_string(),
        n,
        body: linted.body,
        open: linted.open,
        repaired: linted.repaired,
    })
}

/// Load the current row for an annotation/comment id: `(content_json,
/// retired, branch_id)`. `None` when no row ever held the id.
fn current_row(
    store: &crate::storage::CodeGraphStore,
    id: &str,
) -> Result<Option<(String, bool, String)>, AnnError> {
    let (_guard, conn) = store
        .open_diagnostic_conn()
        .map_err(|e| AnnError::Store(format!("{e:?}")))?;
    crate::storage::runtime()
        .block_on(async {
            let mut rows = conn
                .query(
                    "SELECT content, retired, branch_id FROM concepts WHERE id = ?1",
                    libsql::params![id],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            Ok::<_, macrame::DbError>(rows.next().await.map_err(macrame::DbError::Engine)?.map(
                |row| {
                    (
                        row.get::<String>(0).unwrap_or_default(),
                        row.get::<i64>(1).unwrap_or(0) != 0,
                        row.get::<String>(2).unwrap_or_default(),
                    )
                },
            ))
        })
        .map_err(|e| AnnError::Store(format!("{e:?}")))
}

/// The row's own kind (from its content JSON), or `None` when the payload
/// no longer parses — a corrupt row refuses loudly rather than guessing.
fn row_kind(content: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()?
        .get("kind")?
        .as_str()
        .map(str::to_string)
}

/// What `revise` did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Revised {
    pub id: String,
    pub revised: bool,
    pub repaired: bool,
    pub open: usize,
}

/// Supersede `id` with a re-linted body (toggle = a revise with one item
/// flipped — the checklist's own history answers "who checked what
/// when"). The row's kind picks the gates, so a comment can never be
/// revised into list-shape trouble and an annotation can never smuggle
/// prose. Byte-identical bodies are a no-op (`revised: false` — the
/// ledger's `filter_unchanged` absorbs them, no version minted).
pub fn revise(
    store: &crate::storage::CodeGraphStore,
    id: &str,
    raw_body: &str,
) -> Result<Revised, AnnError> {
    let (kind, about, n) = split_id(id).ok_or_else(|| AnnError::InvalidId(id.to_string()))?;
    let (content, retired, branch) =
        current_row(store, id)?.ok_or_else(|| AnnError::NotFound(id.to_string()))?;
    match row_kind(&content).as_deref() {
        Some(k) if k == kind => {}
        other => {
            return Err(AnnError::WrongKind {
                id: id.to_string(),
                kind: other.unwrap_or("unparseable").to_string(),
            });
        }
    }
    if retired {
        return Err(AnnError::AlreadyRetired(id.to_string()));
    }
    let linted = lint(kind, raw_body)?;
    let mut upsert = macrame::ConceptUpsert::new(id.to_string(), id.to_string())
        .content(content_json(kind, about, n, &linted.body, linted.open))
        .extra(extra_json(kind, about))
        .valid_from(crate::storage::now_iso8601())
        .valid_to(crate::storage::TS_OPEN.to_string())
        .retired(false);
    // Trunk-global rows carry no lineage; round-trip a mint lineage if
    // one ever names the row (the §P5a.5 choke-point rule).
    if !branch.is_empty() {
        upsert = upsert.on_branch(
            macrame::BranchId::new(&branch)
                .map_err(|e| AnnError::Store(format!("bad branch_id on {id:?}: {e:?}")))?,
        );
    }
    let written = store
        .upsert_concepts_bulk(std::slice::from_ref(&upsert))
        .map_err(|e| AnnError::Store(format!("{e:?}")))?;
    Ok(Revised {
        id: id.to_string(),
        revised: written == 1,
        repaired: linted.repaired,
        open: linted.open,
    })
}

/// Soft-retire `id` (retire-first, §7 of the proposal): the row stays as
/// an audit trail and every read keeps serving its versions. A second
/// retire is a no-op report, not an error — idempotent like the rest of
/// the boundary.
pub fn retire(store: &crate::storage::CodeGraphStore, id: &str) -> Result<bool, AnnError> {
    let (kind, _, _) = split_id(id).ok_or_else(|| AnnError::InvalidId(id.to_string()))?;
    let (content, retired, _) =
        current_row(store, id)?.ok_or_else(|| AnnError::NotFound(id.to_string()))?;
    match row_kind(&content).as_deref() {
        Some(k) if k == kind => {}
        other => {
            return Err(AnnError::WrongKind {
                id: id.to_string(),
                kind: other.unwrap_or("unparseable").to_string(),
            });
        }
    }
    if retired {
        return Ok(false);
    }
    let (concepts, _) = store
        .retire_entities_at(
            std::slice::from_ref(&id.to_string()),
            &crate::storage::now_iso8601(),
        )
        .map_err(|e| AnnError::Store(format!("{e:?}")))?;
    Ok(concepts == 1)
}

/// One stored version of one annotation/comment row.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AnnVersion {
    pub id: String,
    pub n: u64,
    pub body: String,
    pub open: usize,
    pub valid_from: String,
    pub valid_to: String,
    pub retired: bool,
    pub current: bool,
}

/// One logged version before chain-derivation: `(body, valid_from, retired)`.
type LoggedVersion = (String, String, bool);
/// One row's chain: `(id, head valid_to, head retired, versions)`.
type RowChain = (String, String, bool, Vec<LoggedVersion>);

/// Parse the versions of the given ids, newest-first. The log payloads
/// carry each version's content; contiguity derives from chain order
/// (the `history_of` pattern — a superseded version's payload still
/// names the open sentinel, so intervals derive, they are not stored).
/// The `open` count re-derives from each version's stored body rather
/// than trusting its content JSON — the count is a pure function of the
/// canonical bytes.
fn versions_of(
    store: &crate::storage::CodeGraphStore,
    ids: &[String],
) -> Result<Vec<AnnVersion>, AnnError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let (_guard, conn) = store
        .open_diagnostic_conn()
        .map_err(|e| AnnError::Store(format!("{e:?}")))?;
    crate::storage::runtime()
        .block_on(async {
            let mut per_id: Vec<RowChain> = Vec::new();
            for id in ids {
                // The head lookup's stream must drop before the log
                // stream opens: one live stream per connection, and a
                // second query behind an open stream waits forever.
                let head: Option<(String, bool)> = {
                    let mut rows = conn
                        .query(
                            "SELECT valid_to, retired FROM concepts WHERE id = ?1",
                            libsql::params![id.as_str()],
                        )
                        .await
                        .map_err(macrame::DbError::Engine)?;
                    rows.next()
                        .await
                        .map_err(macrame::DbError::Engine)?
                        .map(|row| {
                            (
                                row.get::<String>(0).unwrap_or_default(),
                                row.get::<i64>(1).unwrap_or(0) != 0,
                            )
                        })
                };
                let (valid_to, retired) = match head {
                    Some(h) => h,
                    None => continue,
                };
                let mut versions = Vec::new();
                let mut log_rows = conn
                    .query(
                        "SELECT payload FROM transaction_log \
                         WHERE table_name = 'concepts' AND entity_id = ?1 \
                         ORDER BY recorded_at DESC",
                        libsql::params![id.as_str()],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                while let Some(row) = log_rows.next().await.map_err(macrame::DbError::Engine)? {
                    let payload: String = row.get(0).unwrap_or_default();
                    let v: serde_json::Value = serde_json::from_str(&payload).map_err(|e| {
                        macrame::DbError::Engine(libsql::Error::Misuse(format!(
                            "annotation log payload for {id:?} does not parse: {e}"
                        )))
                    })?;
                    let content: serde_json::Value = serde_json::from_str(
                        v.get("content").and_then(|c| c.as_str()).unwrap_or("{}"),
                    )
                    .map_err(|e| {
                        macrame::DbError::Engine(libsql::Error::Misuse(format!(
                            "annotation content for {id:?} does not parse: {e}"
                        )))
                    })?;
                    versions.push((
                        content
                            .get("body")
                            .and_then(|b| b.as_str())
                            .unwrap_or("")
                            .to_string(),
                        v.get("valid_from")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string(),
                        v.get("retired").and_then(|r| r.as_i64()).unwrap_or(0) != 0,
                    ));
                }
                per_id.push((id.clone(), valid_to, retired, versions));
            }
            let mut out = Vec::new();
            for (id, head_valid_to, head_retired, versions) in &per_id {
                let (_, _, n) = split_id(id).ok_or_else(|| {
                    macrame::DbError::Engine(libsql::Error::Misuse(format!(
                        "annotation id {id:?} no longer parses"
                    )))
                })?;
                for (i, (body, valid_from, retired)) in versions.iter().enumerate() {
                    out.push(AnnVersion {
                        id: id.clone(),
                        n,
                        body: body.clone(),
                        open: body_open(body),
                        valid_from: valid_from.clone(),
                        valid_to: if i == 0 {
                            head_valid_to.clone()
                        } else {
                            versions[i - 1].1.clone()
                        },
                        retired: *retired,
                        current: i == 0 && !head_retired,
                    });
                }
            }
            // Newest-first across rows: the read contract (Q-B decided —
            // versions are stored, the current one reads first).
            out.sort_by(|a, b| b.valid_from.cmp(&a.valid_from));
            Ok::<_, macrame::DbError>(out)
        })
        .map_err(|e| AnnError::Store(format!("{e:?}")))
}

/// Unchecked task-item count of a stored body. Stored bodies are
/// canonical linter output, so a line scan is exact: `- [ ]` opens an
/// item, `- [x]` closes one.
fn body_open(body: &str) -> usize {
    body.lines()
        .filter(|l| l.trim_start().starts_with("- [ ]"))
        .count()
}

/// Every version of every annotation on `about`, newest-first.
pub fn versions_for(
    store: &crate::storage::CodeGraphStore,
    kind: &str,
    about: &str,
) -> Result<Vec<AnnVersion>, AnnError> {
    if kind != ANNOTATION_KIND && kind != COMMENT_KIND {
        return Err(AnnError::Store(format!("unknown annotation kind {kind:?}")));
    }
    validate_about(about)?;
    let prefix = format!("{kind}:{about}:");
    let pattern = format!("{}%", escape_like(&prefix));
    // Single bound param with the kind as a SQL literal — the exact
    // `history_of` shape. (`kind` is validated to the two constants
    // above, so inlining it is not injection.) The escape is spelled
    // `char(92)` rather than a backslash literal: identical semantics,
    // zero ambiguity about how many backslashes the source holds.
    let sql = format!(
        "SELECT id FROM concepts WHERE id LIKE ?1 ESCAPE char(92) \
         AND json_extract(content, '$.kind') = '{kind}'"
    );
    let mut ids: Vec<String> = {
        let (_guard, conn) = store
            .open_diagnostic_conn()
            .map_err(|e| AnnError::Store(format!("{e:?}")))?;
        crate::storage::runtime()
            .block_on(async {
                let mut out = Vec::new();
                let mut rows = conn
                    .query(&sql, libsql::params![pattern])
                    .await
                    .map_err(macrame::DbError::Engine)?;
                while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                    out.push(row.get::<String>(0).unwrap_or_default());
                }
                Ok::<_, macrame::DbError>(out)
            })
            .map_err(|e| AnnError::Store(format!("{e:?}")))?
    };
    // The LIKE arm shares the prefix hazard from `next_sequence`
    // (`annotation:x:%` also matches `annotation:x:y:1`): keep exact-about
    // ids only. The kind predicate already excludes cross-kind lookalikes.
    ids.retain(|id| id.starts_with(&prefix));
    versions_of(store, &ids)
}

/// Every version of one annotation/comment row, newest-first. Unknown ids
/// read `[]` — the `history_of` honesty, not an error.
pub fn history(
    store: &crate::storage::CodeGraphStore,
    id: &str,
) -> Result<Vec<AnnVersion>, AnnError> {
    let (kind, _, _) = split_id(id).ok_or_else(|| AnnError::InvalidId(id.to_string()))?;
    let row = current_row(store, id)?;
    match row {
        None => Ok(Vec::new()),
        Some((content, _, _)) => match row_kind(&content).as_deref() {
            Some(k) if k == kind => versions_of(store, std::slice::from_ref(&id.to_string())),
            other => Err(AnnError::WrongKind {
                id: id.to_string(),
                kind: other.unwrap_or("unparseable").to_string(),
            }),
        },
    }
}

#[cfg(test)]
mod lint_tests {
    use super::*;

    fn ann(body: &str) -> Result<Linted, LintFault> {
        lint(ANNOTATION_KIND, body)
    }

    fn com(body: &str) -> Result<Linted, LintFault> {
        lint(COMMENT_KIND, body)
    }

    #[test]
    fn plain_bullets_pass_unrepaired() {
        let l = ann("- alpha\n- beta\n").unwrap();
        assert_eq!(l.body, "- alpha\n- beta\n");
        assert_eq!(l.open, 0);
        assert!(!l.repaired);
    }

    #[test]
    fn task_counts_and_toggle_shape() {
        let l = ann("- [ ] one\n- [x] two\n- [ ] three\n").unwrap();
        assert_eq!(l.open, 2);
        assert_eq!(l.body, "- [ ] one\n- [x] two\n- [ ] three\n");
    }

    #[test]
    fn marker_canonicalization_repairs() {
        let l = ann("* alpha\n+beta\n1) gamma\n2)delta\n").unwrap();
        assert!(l.repaired);
        // Adjacent lists of different markers stay two lists: the blank
        // between them is load-bearing (without it they would merge on
        // re-parse), so the canonical form keeps it.
        assert_eq!(l.body, "- alpha\n- beta\n\n1. gamma\n2. delta\n");
    }

    #[test]
    fn trailing_whitespace_and_tabs_repair() {
        // Tabs outside fences expand to tab stops; a tab that would nest
        // a list stays a refusal (flat v1) — so pin the repair where it
        // is legal: mid-line.
        let l = ann("- alpha   \n- b\tc\n").unwrap();
        assert!(l.repaired);
        // The tab sits at column 3, so one space reaches the tab stop.
        assert_eq!(l.body, "- alpha\n- b c\n");
    }

    #[test]
    fn unclosed_fence_closes() {
        let l = com("note\n```rust\nlet x = 1;\n").unwrap();
        assert!(l.repaired);
        assert_eq!(l.body, "note\n\n```rust\nlet x = 1;\n```\n");
    }

    #[test]
    fn fence_contents_are_opaque_to_repairs() {
        // A tab inside code stays a tab; `-x` inside code is not a list.
        let l = com("```\n\t-x\n```\n").unwrap();
        assert!(l.body.contains('\t'));
    }

    #[test]
    fn ordered_list_after_text_gets_its_blank_line() {
        // The repair inserts the separator (comments render para + list
        // instead of merged text), but leading prose still refuses for
        // annotations: Gate 2 sees the paragraph, correctly.
        let l = com("intro\n1. first\n2. second\n").unwrap();
        assert!(l.repaired);
        assert_eq!(l.body, "intro\n\n1. first\n2. second\n");
        let err = ann("intro\n1. first\n2. second\n").unwrap_err();
        assert!(matches!(err, LintFault::NotAList { line: 1, .. }));
    }

    #[test]
    fn crlf_normalizes() {
        let l = ann("- a\r\n- b\r\n").unwrap();
        assert!(l.repaired);
        assert_eq!(l.body, "- a\n- b\n");
    }

    #[test]
    fn grammar_refuses_prose_with_line_and_snippet() {
        let err = ann("- ok\n\njust a paragraph\n").unwrap_err();
        match err {
            LintFault::NotAList { line, snippet } => {
                assert_eq!(line, 3);
                assert!(snippet.contains("just a paragraph"));
            }
            other => panic!("wrong fault: {other:?}"),
        }
    }

    #[test]
    fn grammar_refuses_headings_and_code() {
        // The blank-line repair fires first, so the heading sits on line
        // 3 of the repaired input the fault cites.
        assert!(matches!(
            ann("- ok\n# heading\n"),
            Err(LintFault::NotAList { line: 3, .. })
        ));
        assert!(matches!(
            ann("- ok\n```\nx\n```\n"),
            Err(LintFault::NotAList { .. })
        ));
    }

    #[test]
    fn grammar_refuses_nested_lists() {
        let err = ann("- outer\n  - inner\n").unwrap_err();
        assert!(matches!(err, LintFault::NestedList { line: 2, .. }));
    }

    #[test]
    fn quoted_list_is_not_nesting() {
        // A list under a blockquote is quoted content, not nesting — but
        // it is still not a top-level list, so annotations refuse it as
        // NotAList (the quote), never as NestedList.
        let err = ann("> - quoted\n").unwrap_err();
        assert!(matches!(err, LintFault::NotAList { .. }));
    }

    #[test]
    fn comments_take_prose_code_and_headings() {
        let l = com("# Why\n\nBecause reasons.\n\n```py\nx = 1\n```\n").unwrap();
        assert_eq!(l.open, 0);
        assert!(l.body.contains("# Why"));
        assert!(l.body.contains("```py"));
    }

    #[test]
    fn empty_refuses_both_kinds() {
        assert!(matches!(ann("   \n  \n"), Err(LintFault::Empty)));
        assert!(matches!(com(""), Err(LintFault::Empty)));
    }

    #[test]
    fn canonical_is_a_fixpoint() {
        // Stored bytes re-lint byte-identical: the renderer output must
        // re-parse to events rendering the same bytes. Nasty fixtures
        // included (escapes, html, links, tables, hard breaks, quotes).
        let fixtures = [
            "- [ ] do `code` and *emph* and **strong**\n- [x] done\n",
            "- item with [link](https://example.com/x \"t\") and ![img](i.png)\n",
            "- a <b>tag</b> &amp; entity\n",
            "- hard\\n  break\n",
            "# Head\n\npara with # hash and - dash starts\n",
            "> quoted **bold**\n>\n> - quoted item\n",
            "```rust\nlet tick = \"```\";\n```\n",
            "| a | b |\n|---|---|\n| 1 | 2 |\n",
            "- 1. nested-looking text stays flat\n",
            "- [ ] \\- escaped dash, \\# hash, \\[ bracket\n",
        ];
        for (i, fixture) in fixtures.iter().enumerate() {
            for kind in [ANNOTATION_KIND, COMMENT_KIND] {
                let once = match lint(kind, fixture) {
                    Ok(l) => l,
                    Err(_) => continue, // grammar refusals are not fixpoint subjects
                };
                let twice = lint(kind, &once.body).unwrap_or_else(|e| {
                    panic!("fixture {i} ({kind}) canonical form refuses: {e}");
                });
                assert_eq!(
                    once.body, twice.body,
                    "fixture {i} ({kind}) is not a fixpoint:\n{:?}\nvs\n{:?}",
                    once.body, twice.body
                );
            }
        }
    }

    #[test]
    fn split_id_shapes() {
        assert_eq!(
            split_id("annotation:a.py::g:2"),
            Some(("annotation", "a.py::g", 2))
        );
        assert_eq!(
            split_id("comment:branch:main:content:a.py:12"),
            Some(("comment", "branch:main:content:a.py", 12))
        );
        assert_eq!(split_id("commit:abc"), None);
        assert_eq!(split_id("annotation:x"), None);
        assert_eq!(split_id("annotation::1"), None);
        assert_eq!(split_id("a.py::g"), None);
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::storage::CodeGraphStore;

    fn test_store(name: &str) -> (tempfile::TempDir, CodeGraphStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join(name)).unwrap();
        (dir, store)
    }

    #[test]
    fn create_read_round_trip() {
        let (_dir, store) = test_store("ann1.db");
        let c = create(&store, ANNOTATION_KIND, "a.py::g", "- [ ] do\n- [x] did\n").unwrap();
        assert_eq!(c.id, "annotation:a.py::g:1");
        assert_eq!(c.n, 1);
        assert_eq!(c.open, 1);
        let versions = versions_for(&store, ANNOTATION_KIND, "a.py::g").unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].body, "- [ ] do\n- [x] did\n");
        assert!(versions[0].current);
        assert!(!versions[0].retired);
        assert!(versions[0].valid_from.ends_with('Z'));
        // kinds never mix: the comment scan stays empty.
        assert!(versions_for(&store, COMMENT_KIND, "a.py::g")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn sequences_never_reuse_retired_ids() {
        let (_dir, store) = test_store("ann2.db");
        let a = create(&store, ANNOTATION_KIND, "a.py::g", "- one\n").unwrap();
        assert!(retire(&store, &a.id).unwrap());
        let b = create(&store, ANNOTATION_KIND, "a.py::g", "- two\n").unwrap();
        assert_eq!(b.n, 2);
        assert_eq!(b.id, "annotation:a.py::g:2");
    }

    #[test]
    fn toggle_is_a_version_with_history() {
        let (_dir, store) = test_store("ann3.db");
        let c = create(&store, ANNOTATION_KIND, "a.py::g", "- [ ] task\n").unwrap();
        assert_eq!(c.open, 1);
        let r = revise(&store, &c.id, "- [x] task\n").unwrap();
        assert!(r.revised);
        assert_eq!(r.open, 0);
        let chain = history(&store, &c.id).unwrap();
        assert_eq!(chain.len(), 2);
        // Newest first (Q-B decided): the current version reads first.
        assert!(chain[0].current);
        assert_eq!(chain[0].open, 0);
        assert!(!chain[1].current);
        assert_eq!(chain[1].open, 1);
        // Contiguity derives: the older version ends where the newer starts.
        assert_eq!(chain[1].valid_to, chain[0].valid_from);
    }

    #[test]
    fn identical_revise_is_a_noop() {
        let (_dir, store) = test_store("ann4.db");
        let c = create(&store, COMMENT_KIND, "a.py::g", "rationale\n").unwrap();
        let r = revise(&store, &c.id, "rationale\n").unwrap();
        assert!(!r.revised);
        assert_eq!(history(&store, &c.id).unwrap().len(), 1);
    }

    #[test]
    fn retire_first_then_closed() {
        let (_dir, store) = test_store("ann5.db");
        let c = create(&store, ANNOTATION_KIND, "a.py::g", "- one\n").unwrap();
        assert!(retire(&store, &c.id).unwrap());
        assert!(!retire(&store, &c.id).unwrap()); // idempotent no-op
        let chain = history(&store, &c.id).unwrap();
        assert_eq!(chain.len(), 2);
        assert!(chain[0].retired);
        assert!(!chain[0].current);
        assert!(matches!(
            revise(&store, &c.id, "- two\n"),
            Err(AnnError::AlreadyRetired(_))
        ));
        // …but the audit trail still reads.
        assert_eq!(
            versions_for(&store, ANNOTATION_KIND, "a.py::g")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn user_writes_never_touch_code_rows() {
        let (_dir, store) = test_store("ann6.db");
        store
            .upsert_concepts_bulk(&[macrame::ConceptUpsert::new("a.py::g", "a.py::g")
                .content(r#"{"meta_version":2,"kind":"function"}"#.to_string())
                .valid_from(crate::storage::now_iso8601())
                .valid_to(crate::storage::TS_OPEN.to_string())
                .retired(false)])
            .unwrap();
        assert!(matches!(
            revise(&store, "a.py::g", "- x\n"),
            Err(AnnError::InvalidId(_))
        ));
        assert!(matches!(
            retire(&store, "a.py::g"),
            Err(AnnError::InvalidId(_))
        ));
        // Malformed ids refuse; well-formed-but-absent ids read empty.
        assert!(history(&store, "annotation:nope:1").unwrap().is_empty());
    }

    #[test]
    fn wrong_kind_refuses_loudly() {
        let (_dir, store) = test_store("ann7.db");
        // A row wearing an annotation id but a code kind (hand-minted or
        // corrupt): the kind guard fires before any write.
        store
            .upsert_concepts_bulk(&[macrame::ConceptUpsert::new(
                "annotation:a.py::g:1",
                "annotation:a.py::g:1",
            )
            .content(r#"{"meta_version":2,"kind":"function"}"#.to_string())
            .valid_from(crate::storage::now_iso8601())
            .valid_to(crate::storage::TS_OPEN.to_string())
            .retired(false)])
            .unwrap();
        assert!(matches!(
            revise(&store, "annotation:a.py::g:1", "- x\n"),
            Err(AnnError::WrongKind { .. })
        ));
        assert!(matches!(
            retire(&store, "annotation:a.py::g:1"),
            Err(AnnError::WrongKind { .. })
        ));
    }

    #[test]
    fn nesting_and_empties_refuse() {
        let (_dir, store) = test_store("ann8.db");
        assert!(matches!(
            create(&store, ANNOTATION_KIND, "annotation:a.py::g:1", "- x\n"),
            Err(AnnError::InvalidAbout(_))
        ));
        assert!(matches!(
            create(&store, COMMENT_KIND, "", "x\n"),
            Err(AnnError::InvalidAbout(_))
        ));
        assert!(matches!(
            create(&store, ANNOTATION_KIND, "a.py::g", "   \n"),
            Err(AnnError::Lint(LintFault::Empty))
        ));
        assert!(matches!(
            history(&store, "bogus"),
            Err(AnnError::InvalidId(_))
        ));
    }

    #[test]
    fn infrastructure_excludes_both_kinds() {
        let (_dir, store) = test_store("ann9.db");
        create(&store, ANNOTATION_KIND, "a.py::g", "- x\n").unwrap();
        create(&store, COMMENT_KIND, "a.py::g", "why\n").unwrap();
        for id in store.live_concept_ids().unwrap() {
            assert!(
                !id.starts_with("annotation:") && !id.starts_with("comment:"),
                "sweep universe leaked {id:?}"
            );
        }
        for id in store.all_concept_ids("main").unwrap() {
            assert!(
                !id.starts_with("annotation:") && !id.starts_with("comment:"),
                "diff universe leaked {id:?}"
            );
        }
    }
}
