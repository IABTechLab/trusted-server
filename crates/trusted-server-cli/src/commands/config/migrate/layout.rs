//! Places tables and comments in a `toml_edit` document, so that a rewrite
//! keeps the order and the comments the operator wrote.
//!
//! `toml_edit` writes each table at its position, and holds the comment
//! lines written between two tables with the second, so a table put between
//! two others is put before the comment lines that follow the first one's
//! last setting, as they belong with it. A comment paragraph is a run of
//! comment lines ended by a blank line.

use std::collections::BTreeSet;

use toml_edit::{DocumentMut, Item, RawString, Table};

/// The gap left between the positions of the tables the document was read
/// with, so new tables fit between two of them.
const STRIDE: isize = 1 << 12;

/// The start of the root key that holds a table while it waits to be
/// written as a comment block.
const HELD_PREFIX: &str = "__ts_config_migrate_held_";

/// One step of the path to a table, a key or the index of an entry in an
/// array of tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Key(String),
    Index(usize),
}

/// The path from the root to a table.
pub(super) type TablePath = Vec<Step>;

/// The path of the table at `keys`, below the root.
pub(super) fn path(keys: &[&str]) -> TablePath {
    keys.iter()
        .map(|key| Step::Key((*key).to_owned()))
        .collect()
}

/// A table taken out of the document to be written as a comment block at
/// its place, under the name the stack reads it by.
struct Held {
    key: String,
    header: Option<Vec<String>>,
}

/// The document being rewritten, with the tables that wait to be written as
/// comments.
pub(super) struct Layout {
    pub(super) doc: DocumentMut,
    held: Vec<Held>,
}

impl Layout {
    /// The document read from `doc`, with room left between its tables.
    pub(super) fn new(mut doc: DocumentMut) -> Self {
        spread_positions(doc.as_table_mut());
        Self {
            doc,
            held: Vec::new(),
        }
    }

    /// The table at `path`.
    pub(super) fn table_mut(&mut self, path: &[Step]) -> Option<&mut Table> {
        table_at_mut(self.doc.as_table_mut(), path)
    }

    /// The position of the table at `path`.
    pub(super) fn position_of(&self, path: &[Step]) -> Option<isize> {
        table_at(self.doc.as_table(), path).and_then(Table::position)
    }

    /// The last position the document writes a table at.
    pub(super) fn last_position(&self) -> isize {
        render_order(self.doc.as_table())
            .last()
            .map_or(0, |(position, _)| *position)
    }

    /// The largest position of the table at `path` and every table below it.
    pub(super) fn subtree_end(&self, path: &[Step]) -> Option<isize> {
        let anchor = self.position_of(path)?;
        let end = render_order(self.doc.as_table())
            .into_iter()
            .filter(|(_, table)| table.starts_with(path))
            .map(|(position, _)| position)
            .max();
        Some(end.map_or(anchor, |end| end.max(anchor)))
    }

    /// Writes the tables at `new`, in order, directly after the table at
    /// position `anchor` and before the next one, and moves the comment lines
    /// that follow the anchor's last setting ahead of the first of them.
    ///
    /// Each path in `new` names a table already in the document, a parent
    /// ahead of its children.
    pub(super) fn place_after(&mut self, anchor: isize, new: &[TablePath]) {
        let order = render_order(self.doc.as_table());
        let used: BTreeSet<isize> = order.iter().map(|(position, _)| *position).collect();
        let next = order
            .iter()
            .filter(|(position, table)| *position > anchor && !new.contains(table))
            .find(|(_, table)| !table.is_empty() && self.is_visible(table))
            .map(|(_, table)| table.clone());

        let mut position = anchor;
        for table in new {
            position += 1;
            while used.contains(&position) {
                position += 1;
            }
            if let Some(table) = self.table_mut(table) {
                table.set_position(position);
            }
        }

        let Some(first) = new.first() else {
            return;
        };
        let leading = match &next {
            Some(next) => self.table_mut(next).map_or_else(String::new, |table| {
                let prefix = prefix_of(table);
                let (leading, rest) = split_leading_paragraph(&prefix);
                let leading = leading.to_owned();
                let rest = rest.to_owned();
                if !leading.is_empty() {
                    set_prefix(table, &rest);
                }
                leading
            }),
            None => {
                let trailing = self.doc.trailing().as_str().unwrap_or("").to_owned();
                let (leading, rest) = split_leading_paragraph(&trailing);
                let leading = leading.to_owned();
                if !leading.is_empty() {
                    self.doc.set_trailing(rest.to_owned());
                }
                leading
            }
        };
        if leading.is_empty() {
            return;
        }
        if let Some(table) = self.table_mut(first) {
            let own = prefix_or_blank_line(table);
            set_prefix(table, &format!("{leading}{own}"));
        }
    }

    /// Takes the table at `path` out of the document, with its comments and
    /// its position, to be put back elsewhere.
    pub(super) fn take(&mut self, path: &[Step]) -> Option<Table> {
        take_table(self.doc.as_table_mut(), path)
    }

    /// Turns an inline table at `path` into a table with a header of its
    /// own, so the rules move every table the same way.
    pub(super) fn make_table(&mut self, path: &[Step]) {
        let Some((Step::Key(key), parent)) = path.split_last() else {
            return;
        };
        let Some(item) =
            table_at_mut(self.doc.as_table_mut(), parent).and_then(|parent| parent.get_mut(key))
        else {
            return;
        };
        if item.is_inline_table() {
            let inline = std::mem::take(item);
            *item = inline.into_table().map_or_else(|kept| kept, Item::Table);
        }
    }

    /// Removes the table at `path`, with the tables below it, and gives the
    /// comment paragraphs written above its own to the table written next,
    /// so the table takes only the comments that describe it.
    pub(super) fn remove(&mut self, path: &[Step]) -> Option<Table> {
        let position = self.position_of(path);
        let table = take_table(self.doc.as_table_mut(), path)?;
        let prefix = prefix_of(&table);
        let (head, _own) = split_trailing_paragraph(&prefix);
        if head.lines().any(|line| !line.trim().is_empty()) {
            self.prepend_to_next(position.unwrap_or(0), head);
        }
        Some(table)
    }

    /// Takes the table at `path` out of the document to be written in its
    /// place as a comment block, headed `[<header>]` when `header` is set
    /// and the comments above the table alone otherwise.
    ///
    /// The table waits at the root, at its position, until
    /// [`Self::finish`], so a table placed after it still finds the comment
    /// lines above it.
    pub(super) fn hold(&mut self, path: &[Step], header: Option<&[&str]>) {
        let Some(table) = take_table(self.doc.as_table_mut(), path) else {
            return;
        };
        let key = format!("{HELD_PREFIX}{}", self.held.len());
        self.doc.as_table_mut().insert(&key, Item::Table(table));
        self.held.push(Held {
            key,
            header: header.map(|parts| parts.iter().map(|part| (*part).to_owned()).collect()),
        });
    }

    /// The document as text, with each held table written as comments in
    /// its place.
    pub(super) fn finish(mut self) -> String {
        let mut blocks = Vec::new();
        for held in &self.held {
            let Some(Item::Table(table)) = self.doc.as_table_mut().remove(&held.key) else {
                continue;
            };
            let mut marker = Table::new();
            if let Some(position) = table.position() {
                marker.set_position(position);
            }
            marker.decor_mut().set_prefix("");
            marker.decor_mut().set_suffix("");
            self.doc
                .as_table_mut()
                .insert(&held.key, Item::Table(marker));
            blocks.push((
                format!("[{}]\n", held.key),
                commented_block(table, held.header.as_deref()),
            ));
        }
        let mut text = self.doc.to_string();
        for (marker, block) in blocks {
            text = text.replacen(&marker, &block, 1);
        }
        text
    }

    /// Whether the table at `path` writes a header, which an implicit table
    /// with no settings of its own does not.
    fn is_visible(&self, path: &[Step]) -> bool {
        table_at(self.doc.as_table(), path)
            .is_some_and(|table| !(table.is_implicit() && table.get_values().is_empty()))
    }

    /// Puts `text` ahead of the prefix of the first table written after
    /// `position`, or ahead of the document's trailing text when none is.
    fn prepend_to_next(&mut self, position: isize, text: &str) {
        let next = render_order(self.doc.as_table())
            .into_iter()
            .filter(|(at, table)| *at >= position && !table.is_empty())
            .find(|(_, table)| self.is_visible(table))
            .map(|(_, table)| table);
        match next.and_then(|next| self.table_mut(&next)) {
            Some(table) => {
                let prefix = prefix_of(table);
                set_prefix(table, &format!("{text}{prefix}"));
            }
            None => {
                let trailing = self.doc.trailing().as_str().unwrap_or("").to_owned();
                self.doc.set_trailing(format!("{text}{trailing}"));
            }
        }
    }
}

/// The text written ahead of a table's header.
pub(super) fn prefix_of(table: &Table) -> String {
    table
        .decor()
        .prefix()
        .and_then(RawString::as_str)
        .unwrap_or("")
        .to_owned()
}

/// Sets the text written ahead of a table's header.
pub(super) fn set_prefix(table: &mut Table, text: &str) {
    table.decor_mut().set_prefix(text.to_owned());
}

/// The text ahead of a table's header, or a blank line where none was
/// written, which is what `toml_edit` writes ahead of a table it made.
fn prefix_or_blank_line(table: &Table) -> String {
    table
        .decor()
        .prefix()
        .and_then(RawString::as_str)
        .map_or_else(|| "\n".to_owned(), str::to_owned)
}

/// Splits off the comment lines at the start of `text` that a blank line
/// separates from the rest. Those lines follow the setting written above
/// them, so they belong with it. Text that starts with a blank line, or that
/// has none, has no such lines.
pub(super) fn split_leading_paragraph(text: &str) -> (&str, &str) {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            return text.split_at(offset);
        }
        offset += line.len();
    }
    ("", text)
}

/// Splits off the comment lines at the end of `text`, directly above what
/// follows it, from the text ahead of them.
pub(super) fn split_trailing_paragraph(text: &str) -> (&str, &str) {
    let mut start = 0;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.trim().is_empty() {
            start = offset;
        }
    }
    text.split_at(start)
}

/// Multiplies the position of every table by [`STRIDE`].
fn spread_positions(table: &mut Table) {
    if let Some(position) = table.position() {
        table.set_position(position.saturating_mul(STRIDE));
    }
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(child) => spread_positions(child),
            Item::ArrayOfTables(array) => {
                for child in array.iter_mut() {
                    spread_positions(child);
                }
            }
            Item::None | Item::Value(_) => {}
        }
    }
}

/// Every table below `root`, with the position `toml_edit` sorts it by, in
/// the order it writes them. A table with no position takes the position of
/// the table visited before it, as `toml_edit` does.
fn render_order(root: &Table) -> Vec<(isize, TablePath)> {
    fn visit(
        table: &Table,
        path: &mut TablePath,
        last: &mut isize,
        out: &mut Vec<(isize, TablePath)>,
    ) {
        if !table.is_dotted() {
            if let Some(position) = table.position() {
                *last = position;
            }
            out.push((*last, path.clone()));
        }
        for (key, item) in table.iter() {
            match item {
                Item::Table(child) => {
                    path.push(Step::Key(key.to_owned()));
                    visit(child, path, last, out);
                    path.pop();
                }
                Item::ArrayOfTables(array) => {
                    for (index, child) in array.iter().enumerate() {
                        path.push(Step::Key(key.to_owned()));
                        path.push(Step::Index(index));
                        visit(child, path, last, out);
                        path.pop();
                        path.pop();
                    }
                }
                Item::None | Item::Value(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    visit(root, &mut Vec::new(), &mut 0, &mut out);
    out.sort_by_key(|(position, _)| *position);
    out
}

fn table_at<'a>(root: &'a Table, path: &[Step]) -> Option<&'a Table> {
    let mut table = root;
    let mut steps = path.iter();
    while let Some(step) = steps.next() {
        let Step::Key(key) = step else {
            return None;
        };
        table = match table.get(key)? {
            Item::Table(child) => child,
            Item::ArrayOfTables(array) => {
                let Some(Step::Index(index)) = steps.next() else {
                    return None;
                };
                array.get(*index)?
            }
            Item::None | Item::Value(_) => return None,
        };
    }
    Some(table)
}

fn table_at_mut<'a>(root: &'a mut Table, path: &[Step]) -> Option<&'a mut Table> {
    let mut table = root;
    let mut steps = path.iter();
    while let Some(step) = steps.next() {
        let Step::Key(key) = step else {
            return None;
        };
        table = match table.get_mut(key)? {
            Item::Table(child) => child,
            Item::ArrayOfTables(array) => {
                let Some(Step::Index(index)) = steps.next() else {
                    return None;
                };
                array.get_mut(*index)?
            }
            Item::None | Item::Value(_) => return None,
        };
    }
    Some(table)
}

/// Takes the table at `path` out of its parent, and an entry out of its
/// array of tables, dropping the array when it is left empty.
fn take_table(root: &mut Table, path: &[Step]) -> Option<Table> {
    match path.split_last()? {
        (Step::Key(key), parent) => {
            let parent = table_at_mut(root, parent)?;
            if !parent.get(key)?.is_table() {
                return None;
            }
            parent.remove(key).and_then(|item| item.into_table().ok())
        }
        (Step::Index(index), parent) => {
            let (Step::Key(key), grandparent) = parent.split_last()? else {
                return None;
            };
            let grandparent = table_at_mut(root, grandparent)?;
            let array = grandparent.get_mut(key)?.as_array_of_tables_mut()?;
            if *index >= array.len() {
                return None;
            }
            let table = array.remove(*index);
            if array.is_empty() {
                grandparent.remove(key);
            }
            Some(table)
        }
    }
}

/// `table` written as comments: the comments above it, and, under
/// `header`, its settings and the tables below it with each line commented
/// out. Lines that are already comments, and blank lines, are kept as they
/// are.
fn commented_block(table: Table, header: Option<&[String]>) -> String {
    let prefix = prefix_of(&table);
    let Some(header) = header else {
        return prefix;
    };
    let mut table = table;
    set_prefix(&mut table, "");
    table.set_implicit(false);
    let mut mini = DocumentMut::new();
    let mut parent = mini.as_table_mut();
    let Some((last, parents)) = header.split_last() else {
        return prefix;
    };
    for part in parents {
        let entry = parent.entry(part).or_insert_with(|| {
            let mut implicit = Table::new();
            implicit.set_implicit(true);
            Item::Table(implicit)
        });
        let Some(next) = entry.as_table_mut() else {
            return prefix;
        };
        parent = next;
    }
    parent.insert(last, Item::Table(table));
    let body = mini.to_string();
    let mut block = prefix;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.trim().is_empty() || trimmed.starts_with('#') {
            block.push_str(line);
        } else {
            block.push_str("# ");
            block.push_str(line);
        }
    }
    block
}

/// Whether `text` ends its first line with a carriage return and a line
/// feed.
pub(super) fn uses_crlf(text: &str) -> bool {
    text.find('\n').is_some_and(|index| {
        index
            .checked_sub(1)
            .and_then(|before| text.as_bytes().get(before))
            == Some(&b'\r')
    })
}

/// `text` with every line feed not already after a carriage return given
/// one, for a document read with Windows line endings.
pub(super) fn to_crlf(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    let mut previous = '\0';
    for character in text.chars() {
        if character == '\n' && previous != '\r' {
            out.push('\r');
        }
        out.push(character);
        previous = character;
    }
    out
}
