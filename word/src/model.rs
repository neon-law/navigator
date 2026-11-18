use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentModel {
    pub protocol_version: u16,
    pub package: PackageInventory,
    pub stories: Vec<Story>,
    pub styles: Vec<StyleDefinition>,
    pub numbering: Vec<NumberingDefinition>,
    pub comments: Vec<Comment>,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    pub revision_nodes: Vec<RevisionNode>,
}

impl DocumentModel {
    /// Resolve the imported Word model into the shared seven-level outline
    /// representation. Diagnostics are returned alongside content so an
    /// attorney-facing caller can refuse ambiguous mappings without losing
    /// the ordered source blocks.
    #[must_use]
    pub fn canonical_outline(&self) -> crate::outline::CanonicalDocument {
        crate::outline::from_stories(&self.stories, &self.numbering, &self.diagnostics)
    }
}

impl DocumentModel {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            protocol_version: crate::PROTOCOL_VERSION,
            package: PackageInventory::default(),
            stories: Vec::new(),
            styles: Vec::new(),
            numbering: Vec::new(),
            comments: Vec::new(),
            diagnostics: Vec::new(),
            revision_nodes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PackageInventory {
    pub parts: Vec<PackagePart>,
    pub relationships: Vec<PackageRelationship>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackagePart {
    pub uri: String,
    pub content_type: String,
    pub relationship_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRelationship {
    pub source_uri: String,
    pub relationship_id: String,
    pub target_uri: String,
    pub relationship_type: String,
    pub external: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Story {
    pub kind: StoryKind,
    pub part_uri: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoryKind {
    MainDocument,
    Header,
    Footer,
    Footnotes,
    Endnotes,
    Comments,
    TextBox,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    SectionBreak {
        break_kind: BreakKind,
        #[serde(default)]
        anchor: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paragraph {
    #[serde(default)]
    pub anchor: String,
    pub style_id: Option<String>,
    pub numbering: Option<NumberingIdentity>,
    pub nodes: Vec<Inline>,
    pub revisions: Vec<RevisionNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    #[serde(default)]
    pub anchor: String,
    pub style_id: Option<String>,
    pub rows: Vec<TableRow>,
    pub revisions: Vec<RevisionNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableCell {
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Inline {
    Text {
        text: String,
        style_id: Option<String>,
        revision: Option<RevisionKind>,
    },
    Tab {
        style_id: Option<String>,
    },
    Break {
        break_kind: BreakKind,
    },
    BookmarkStart {
        id: String,
        name: Option<String>,
    },
    BookmarkEnd {
        id: String,
    },
    Hyperlink {
        relationship_id: Option<String>,
        anchor: Option<String>,
        children: Vec<Inline>,
    },
    Field {
        field_kind: FieldKind,
        instruction: Option<String>,
        children: Vec<Inline>,
    },
    CommentRangeStart {
        id: String,
    },
    CommentRangeEnd {
        id: String,
    },
    CommentReference {
        id: String,
    },
    Revision {
        revision: RevisionNode,
        children: Vec<Inline>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakKind {
    Line,
    Page,
    Column,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Simple,
    Complex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumberingIdentity {
    pub numbering_id: String,
    pub level: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleDefinition {
    pub id: String,
    pub style_type: Option<String>,
    pub based_on: Option<String>,
    pub next_style: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumberingDefinition {
    pub numbering_id: String,
    pub abstract_numbering_id: Option<String>,
    pub levels: Vec<String>,
    #[serde(default)]
    pub level_definitions: Vec<crate::outline::NumberingLevel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionNode {
    pub kind: RevisionKind,
    pub id: Option<String>,
    pub anchor: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionKind {
    Insertion,
    Deletion,
    MoveFrom,
    MoveTo,
    ParagraphProperties,
    RunProperties,
    TableProperties,
    TableGrid,
    TableRowProperties,
    TableCellProperties,
    Numbering,
    SectionProperties,
}

impl RevisionKind {
    #[must_use]
    pub fn is_accepted(self) -> bool {
        matches!(self, Self::Insertion | Self::MoveTo)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: DiagnosticSeverity,
    pub anchor: String,
}

impl Diagnostic {
    #[must_use]
    pub fn unsupported_revision(anchor: &str, kind: &str) -> Self {
        Self {
            code: DiagnosticCode::UnsupportedRevision,
            severity: DiagnosticSeverity::Error,
            anchor: format!("{anchor}:{kind}"),
        }
    }

    #[must_use]
    pub fn unsupported_numbering(anchor: &str) -> Self {
        Self::outline(DiagnosticCode::UnsupportedNumbering, anchor)
    }

    #[must_use]
    pub fn ambiguous_outline(anchor: &str) -> Self {
        Self::outline(DiagnosticCode::AmbiguousOutline, anchor)
    }

    #[must_use]
    pub fn depth_overflow(anchor: &str, depth: u8) -> Self {
        Self {
            code: DiagnosticCode::DepthOverflow,
            severity: DiagnosticSeverity::Error,
            anchor: format!("{anchor}:depth-{depth}"),
        }
    }

    #[must_use]
    pub fn skipped_outline_level(anchor: &str, depth: u8) -> Self {
        Self {
            code: DiagnosticCode::SkippedOutlineLevel,
            severity: DiagnosticSeverity::Error,
            anchor: format!("{anchor}:depth-{depth}"),
        }
    }

    #[must_use]
    pub fn manual_outline_label(anchor: &str) -> Self {
        Self::outline(DiagnosticCode::ManualOutlineLabel, anchor)
    }

    #[must_use]
    pub fn list_restart(anchor: &str) -> Self {
        Self::outline(DiagnosticCode::ListRestart, anchor)
    }

    #[must_use]
    pub fn missing_source_anchor(anchor: &str) -> Self {
        Self::outline(DiagnosticCode::MissingSourceAnchor, anchor)
    }

    fn outline(code: DiagnosticCode, anchor: &str) -> Self {
        Self {
            code,
            severity: DiagnosticSeverity::Warning,
            anchor: anchor.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    ProtocolVersion,
    UnsupportedRevision,
    MissingMainDocument,
    ExternalRelationship,
    CorruptPackage,
    EscapingPackage,
    MacroEnabledPackage,
    EncryptedPackage,
    ZipEntryCountExceeded,
    ZipEntryUncompressedSizeExceeded,
    ZipTotalUncompressedSizeExceeded,
    OpenXmlValidation,
    UnsupportedNumbering,
    AmbiguousOutline,
    DepthOverflow,
    SkippedOutlineLevel,
    ManualOutlineLabel,
    ListRestart,
    MissingSourceAnchor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}
