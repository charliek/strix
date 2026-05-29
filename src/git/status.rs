//! Parsing of `git status --porcelain=v2 --branch -z`.
//!
//! The v2 porcelain format is line-oriented with a leading type byte per
//! record; `-z` makes records NUL-terminated (so paths need no quoting) and
//! turns the rename `<path>\t<orig>` separator into a NUL — meaning a rename
//! record consumes *two* NUL-delimited fields. See `git status` docs.

/// The kind of change to a file, taken from a porcelain v2 XY status code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChange,
    Untracked,
    Conflicted,
}

impl Change {
    /// The single-character marker shown next to the file.
    pub fn marker(self) -> char {
        match self {
            Change::Added => 'A',
            Change::Modified => 'M',
            Change::Deleted => 'D',
            Change::Renamed => 'R',
            Change::Copied => 'C',
            Change::TypeChange => 'T',
            Change::Untracked => '?',
            Change::Conflicted => 'U',
        }
    }
}

/// Which section a file is shown under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Staged,
    Unstaged,
}

/// A single changed file in one section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub orig_path: Option<String>,
    pub change: Change,
}

impl FileEntry {
    fn new(path: String, orig_path: Option<String>, change: Change) -> Self {
        FileEntry {
            path,
            orig_path,
            change,
        }
    }

    /// How the file is labelled in the list (rename shows `orig → path`).
    pub fn display_path(&self) -> String {
        match &self.orig_path {
            Some(orig) => format!("{orig} → {}", self.path),
            None => self.path.clone(),
        }
    }
}

/// The repository status: the current branch plus the staged and unstaged
/// (including untracked) file lists.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub branch: Option<String>,
    pub detached: bool,
    pub head_oid: Option<String>,
    pub staged: Vec<FileEntry>,
    pub unstaged: Vec<FileEntry>,
}

impl Status {
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty()
    }

    pub fn total(&self) -> usize {
        self.staged.len() + self.unstaged.len()
    }

    /// A short label for HEAD: the branch name, or `detached @ <short-oid>`.
    pub fn head_label(&self) -> Option<String> {
        if let Some(branch) = &self.branch {
            Some(branch.clone())
        } else if self.detached {
            Some(match &self.head_oid {
                Some(oid) => format!("detached @ {}", &oid[..oid.len().min(8)]),
                None => "detached".to_string(),
            })
        } else {
            None
        }
    }
}

fn change_from_code(code: char) -> Option<Change> {
    match code {
        'M' => Some(Change::Modified),
        'A' => Some(Change::Added),
        'D' => Some(Change::Deleted),
        'R' => Some(Change::Renamed),
        'C' => Some(Change::Copied),
        'T' => Some(Change::TypeChange),
        'U' => Some(Change::Conflicted),
        _ => None,
    }
}

/// Number of space-separated fields before the path in each porcelain v2 record
/// (`1` ordinary, `2` rename/copy, `u` unmerged).
const ORDINARY_PATH_FIELDS: usize = 8;
const RENAME_PATH_FIELDS: usize = 9;
const UNMERGED_PATH_FIELDS: usize = 10;

/// Returns the substring after `leading` space-separated tokens — the file path,
/// which may itself contain spaces (so we must not split it further).
fn path_after(field: &str, leading: usize) -> String {
    field
        .splitn(leading + 1, ' ')
        .nth(leading)
        .unwrap_or("")
        .to_string()
}

fn xy_codes(field: &str) -> (char, char) {
    let xy = field.split(' ').nth(1).unwrap_or("..");
    let mut chars = xy.chars();
    (chars.next().unwrap_or('.'), chars.next().unwrap_or('.'))
}

/// Parse the raw bytes of `git status --porcelain=v2 --branch -z`.
pub fn parse(bytes: &[u8]) -> Status {
    let mut status = Status::default();
    let mut fields = bytes.split(|&b| b == 0);

    while let Some(raw) = fields.next() {
        if raw.is_empty() {
            continue;
        }
        let owned = String::from_utf8_lossy(raw);
        let field: &str = &owned;
        match field.as_bytes()[0] {
            b'#' => parse_branch_header(field, &mut status),
            b'1' => {
                let (x, y) = xy_codes(field);
                let path = path_after(field, ORDINARY_PATH_FIELDS);
                push_entry(&mut status, x, y, &path, None);
            }
            b'2' => {
                let orig = fields
                    .next()
                    .map(|raw| String::from_utf8_lossy(raw).into_owned());
                let (x, y) = xy_codes(field);
                let path = path_after(field, RENAME_PATH_FIELDS);
                push_entry(&mut status, x, y, &path, orig);
            }
            b'?' => status.unstaged.push(FileEntry::new(
                field[2..].to_string(),
                None,
                Change::Untracked,
            )),
            b'u' => {
                let path = path_after(field, UNMERGED_PATH_FIELDS);
                status
                    .unstaged
                    .push(FileEntry::new(path, None, Change::Conflicted));
            }
            _ => {}
        }
    }

    status
}

fn push_entry(status: &mut Status, x: char, y: char, path: &str, orig: Option<String>) {
    if let Some(change) = change_from_code(x) {
        status
            .staged
            .push(FileEntry::new(path.to_string(), orig.clone(), change));
    }
    if let Some(change) = change_from_code(y) {
        status
            .unstaged
            .push(FileEntry::new(path.to_string(), orig, change));
    }
}

fn parse_branch_header(field: &str, status: &mut Status) {
    if let Some(rest) = field.strip_prefix("# branch.head ") {
        if rest == "(detached)" {
            status.detached = true;
        } else {
            status.branch = Some(rest.to_string());
        }
    } else if let Some(rest) = field.strip_prefix("# branch.oid ") {
        status.head_oid = Some(rest.to_string());
    }
}
