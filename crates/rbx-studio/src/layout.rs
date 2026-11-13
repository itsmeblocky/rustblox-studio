//! Parser for Studio's Qtitan dock layouts.
//!
//! Studio stores its exact panel arrangement in the Windows registry under
//! `HKCU\Software\Roblox\RobloxStudio\LayoutSettings\Docking\3` as three
//! `@ByteArray(...)` XML strings (`edit_`, `play_`, `pserv_`). They have been
//! extracted verbatim to `assets/studio/layout/{edit,play,pserv}.xml`.
//!
//! The XML is a [`DockPanelLayouts`] document:
//!
//! ```xml
//! <DockPanelLayouts>
//!   <Panels Count="136">
//!     <Panel ID="Layout1" Type="4" DockCX="0" DockCY="471" Horizontal="False">
//!       <Panels Count="2"><Panel ID="Layout21"/><Panel ID="Layout45"/></Panels>
//!     </Panel>
//!     <Panel ID="propertiesWidgetPanel" Type="Qtitan::DockWidgetPanel" ...>
//!       <Layouts>
//!         <DockingLayout ID="Layout22"/>
//!         <FloatingLayout ID=""/>
//!         <LastLayout ID="Layout22"/>
//!       </Layouts>
//!     </Panel>
//!   </Panels>
//!   <Windows>...</Windows>
//!   <TopLayout ID="Layout0"/>
//!   <Client ID="Layout1"/>
//! </DockPanelLayouts>
//! ```
//!
//! Numeric `Type` values describe layout nodes: `1` and `4` are splitters,
//! `2` and `5` are tabbed groups. `Qtitan::*` values describe dock content.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use xml::attribute::OwnedAttribute;
use xml::reader::{EventReader, XmlEvent};

/// A rectangle in screen coordinates, as stored for floating dock windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl WindowRect {
    pub fn width(self) -> i32 {
        self.right - self.left
    }

    pub fn height(self) -> i32 {
        self.bottom - self.top
    }
}

/// What a layout node represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    /// A splitter container (Qtitan `Type="1"` / `Type="4"`).
    Split,
    /// A tabbed group of dock panels (Qtitan `Type="2"` / `Type="5"`).
    TabGroup,
    /// A dockable tool panel (`Qtitan::DockWidgetPanel`).
    DockWidget,
    /// A document pane such as the script editor (`Qtitan::DockDocumentPanel`).
    DocumentPanel,
    /// A floating top-level window (`Qtitan::DockWindow`).
    DockWindow,
    /// Unrecognized Qtitan type code.
    Other(u32),
}

impl PanelKind {
    fn from_type(value: &str) -> PanelKind {
        match value {
            "1" | "4" => PanelKind::Split,
            "2" | "5" => PanelKind::TabGroup,
            "Qtitan::DockWidgetPanel" => PanelKind::DockWidget,
            "Qtitan::DockDocumentPanel" => PanelKind::DocumentPanel,
            "Qtitan::DockWindow" => PanelKind::DockWindow,
            other => other
                .parse::<u32>()
                .map(PanelKind::Other)
                .unwrap_or(PanelKind::Other(0)),
        }
    }
}

/// IDs of the three layouts a dock panel can live in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayoutRefs {
    /// The docked tab group the panel currently belongs to.
    pub docking: Option<String>,
    /// The floating layout, when the panel has been torn off.
    pub floating: Option<String>,
    /// The layout that was used last (docking or floating).
    pub last: Option<String>,
}

/// One `<Panel>` entry from the layout document.
#[derive(Debug, Clone, Default)]
pub struct Panel {
    pub id: String,
    pub kind: Option<PanelKind>,
    /// Stored width of this node (`DockCX`), `-1` when unset.
    pub dock_cx: i32,
    /// Stored height of this node (`DockCY`), `-1` when unset.
    pub dock_cy: i32,
    /// Splitter orientation; only meaningful for [`PanelKind::Split`].
    pub horizontal: bool,
    pub active: bool,
    /// Child IDs, in order. For tab groups this is the tab order.
    pub children: Vec<String>,
    /// For dock widgets: the tab group / layout the panel is docked into.
    pub layouts: LayoutRefs,
    /// For [`PanelKind::DockWindow`]: the root layout of the floating window.
    pub top_layout: Option<String>,
    pub window_rect: Option<WindowRect>,
}

impl Panel {
    /// A display-friendly name for anonymous layout nodes.
    pub fn label(&self) -> &str {
        &self.id
    }

    /// True when this node has a stored size.
    pub fn has_dock_size(&self) -> bool {
        self.dock_cx >= 0 && self.dock_cy >= 0
    }
}

/// A parsed Qtitan layout document.
#[derive(Debug, Clone, Default)]
pub struct DockLayout {
    panels: BTreeMap<String, Panel>,
    /// Insertion order of panel definitions, for deterministic dumps.
    order: Vec<String>,
    windows: Vec<String>,
    pub top_layout: Option<String>,
    pub client: Option<String>,
}

impl DockLayout {
    /// Parse an `edit.xml` / `play.xml` / `pserv.xml` file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading dock layout {}", path.display()))?;
        Self::parse(&raw).with_context(|| format!("parsing dock layout {}", path.display()))
    }

    /// Parse the raw registry string (with its `@ByteArray(...)` wrapper).
    pub fn parse(raw: &str) -> Result<Self> {
        let xml = strip_byte_array(raw);
        let mut builder = Builder::default();
        builder.run(xml)?;
        Ok(builder.finish())
    }

    /// Load the bundled layout for edit mode.
    pub fn bundled(mode: LayoutMode) -> Result<Self> {
        Self::load(bundled_path(mode))
    }

    pub fn len(&self) -> usize {
        self.panels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.panels.is_empty()
    }

    pub fn panel(&self, id: &str) -> Option<&Panel> {
        self.panels.get(id)
    }

    pub fn panels(&self) -> impl Iterator<Item = &Panel> {
        self.order.iter().filter_map(move |id| self.panels.get(id))
    }

    pub fn windows(&self) -> impl Iterator<Item = &Panel> {
        self.windows.iter().filter_map(move |id| self.panels.get(id))
    }

    /// Walk the docked layout depth-first from the top layout.
    pub fn walk(&self) -> Vec<&Panel> {
        let mut out = Vec::new();
        if let Some(root) = self.top_layout.as_deref() {
            self.walk_from(root, &mut out);
        }
        out
    }

    fn walk_from<'a>(&'a self, id: &str, out: &mut Vec<&'a Panel>) {
        let Some(panel) = self.panels.get(id) else {
            return;
        };
        out.push(panel);
        for child in &panel.children {
            self.walk_from(child, out);
        }
    }

    /// Tab groups in tree order, i.e. the order Studio lays docks out.
    pub fn tab_groups(&self) -> Vec<&Panel> {
        self.walk()
            .into_iter()
            .filter(|panel| panel.kind == Some(PanelKind::TabGroup))
            .collect()
    }

    /// Dock panels in tree order, optionally only those that are actually docked.
    pub fn dock_widgets(&self, only_docked: bool) -> Vec<&Panel> {
        self.walk()
            .into_iter()
            .filter(|panel| panel.kind == Some(PanelKind::DockWidget))
            .filter(|panel| {
                !only_docked
                    || panel
                        .layouts
                        .docking
                        .as_deref()
                        .is_some_and(|id| !id.is_empty())
            })
            .collect()
    }

    /// Panels shown in a tab group, in tab order.
    pub fn tabs(&self, group_id: &str) -> Vec<&Panel> {
        let Some(group) = self.panels.get(group_id) else {
            return Vec::new();
        };
        group
            .children
            .iter()
            .filter_map(|id| self.panels.get(id))
            .collect()
    }

    /// The tab group a dock widget currently belongs to, if it is docked.
    pub fn group_of(&self, panel_id: &str) -> Option<&Panel> {
        let panel = self.panels.get(panel_id)?;
        let group_id = panel.layouts.docking.as_deref().filter(|id| !id.is_empty())?;
        self.panels.get(group_id)
    }
}

/// Which Studio mode the saved layout belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Edit,
    Play,
    PlayServer,
}

impl LayoutMode {
    pub fn file_stem(self) -> &'static str {
        match self {
            LayoutMode::Edit => "edit",
            LayoutMode::Play => "play",
            LayoutMode::PlayServer => "pserv",
        }
    }
}

/// Strip the `@ByteArray(...)` wrapper QSettings writes around the XML string.
fn strip_byte_array(raw: &str) -> &str {
    let trimmed = raw.trim_start_matches('\u{feff}').trim();
    let inner = trimmed
        .strip_prefix("@ByteArray(")
        .unwrap_or(trimmed);
    inner.strip_suffix(')').unwrap_or(inner)
}

#[derive(Default)]
struct Builder {
    panels: BTreeMap<String, Panel>,
    order: Vec<String>,
    windows: Vec<String>,
    top_layout: Option<String>,
    client: Option<String>,
    current: Option<String>,
    /// One entry per open `<Panel>` element: `true` for definitions that own
    /// `current`, `false` for the `<Panel ID="..."/>` references inside
    /// `<Panels>` lists.
    panel_stack: Vec<bool>,
    in_windows: bool,
    in_panels: bool,
    window_index: usize,
}

impl Builder {
    fn run(&mut self, xml: &str) -> Result<()> {
        let parser = EventReader::new(xml.as_bytes());
        for event in parser {
            match event.with_context(|| "reading dock layout XML")? {
                XmlEvent::StartElement {
                    name, attributes, ..
                } => {
                    let tag = name.local_name.as_str();
                    match tag {
                        "Windows" => self.in_windows = true,
                        "Panels" => {
                            // `<Panels>` appears both as the top-level list and
                            // as the child list of a split/tab group.
                            self.in_panels = true;
                        }
                        "Panel" => self.start_panel(&attributes),
                        "TopLayout" => {
                            if let Some(current) = self.current.as_deref() {
                                if let Some(panel) = self.panels.get_mut(current) {
                                    panel.top_layout = xml_attr(&attributes, "ID").map(str::to_string);
                                }
                            } else {
                                self.top_layout = xml_attr(&attributes, "ID").map(str::to_string);
                            }
                        }
                        "Client" => self.client = xml_attr(&attributes, "ID").map(str::to_string),
                        "DockingLayout" => self.set_layout_ref(xml_attr(&attributes, "ID"), RefKind::Docking),
                        "FloatingLayout" => self.set_layout_ref(xml_attr(&attributes, "ID"), RefKind::Floating),
                        "LastLayout" => self.set_layout_ref(xml_attr(&attributes, "ID"), RefKind::Last),
                        _ => {}
                    }
                }
                XmlEvent::EndElement { name } => match name.local_name.as_str() {
                    "Windows" => self.in_windows = false,
                    "Panels" => self.in_panels = false,
                    "Panel" => {
                        if self.panel_stack.pop().unwrap_or(false) {
                            self.current = None;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(())
    }

    fn start_panel(&mut self, attrs: &[OwnedAttribute]) {
        let id = xml_attr(attrs, "ID");
        let kind = xml_attr(attrs, "Type");
        let Some(kind) = kind else {
            // A `<Panel ID="..."/>` reference inside a `<Panels>` list.
            self.panel_stack.push(false);
            if self.in_panels {
                let Some(child) = id else { return };
                if let Some(current) = self.current.as_deref() {
                    if let Some(panel) = self.panels.get_mut(current) {
                        panel.children.push(child.to_string());
                    }
                }
            }
            return;
        };

        let panel_kind = PanelKind::from_type(kind);
        self.panel_stack.push(true);
        let panel_id = match id {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => {
                self.window_index += 1;
                format!("window#{}", self.window_index)
            }
        };

        let panel = Panel {
            id: panel_id.clone(),
            kind: Some(panel_kind),
            dock_cx: xml_attr(attrs, "DockCX").and_then(|v| v.parse().ok()).unwrap_or(-1),
            dock_cy: xml_attr(attrs, "DockCY").and_then(|v| v.parse().ok()).unwrap_or(-1),
            horizontal: xml_attr(attrs, "Horizontal").is_some_and(|v| v.eq_ignore_ascii_case("true")),
            active: xml_attr(attrs, "Active").is_some_and(|v| v.eq_ignore_ascii_case("true")),
            ..Panel::default()
        };

        if panel_kind == PanelKind::DockWindow {
            let rect = WindowRect {
                left: xml_attr(attrs, "WindowRectLeft").and_then(|v| v.parse().ok()).unwrap_or(0),
                top: xml_attr(attrs, "WindowRectTop").and_then(|v| v.parse().ok()).unwrap_or(0),
                right: xml_attr(attrs, "WindowRectRight").and_then(|v| v.parse().ok()).unwrap_or(0),
                bottom: xml_attr(attrs, "WindowRectBottom").and_then(|v| v.parse().ok()).unwrap_or(0),
            };
            let panel = Panel {
                window_rect: Some(rect),
                ..panel
            };
            self.windows.push(panel_id.clone());
            self.panels.insert(panel_id.clone(), panel);
        } else {
            self.order.push(panel_id.clone());
            self.panels.insert(panel_id.clone(), panel);
        }
        self.current = Some(panel_id);
    }

    fn set_layout_ref(&mut self, id: Option<&str>, kind: RefKind) {
        let Some(current) = self.current.as_deref() else {
            return;
        };
        let value = id.filter(|id| !id.is_empty()).map(str::to_string);
        if let Some(panel) = self.panels.get_mut(current) {
            match kind {
                RefKind::Docking => panel.layouts.docking = value,
                RefKind::Floating => panel.layouts.floating = value,
                RefKind::Last => panel.layouts.last = value,
            }
        }
    }

    fn finish(self) -> DockLayout {
        DockLayout {
            panels: self.panels,
            order: self.order,
            windows: self.windows,
            top_layout: self.top_layout,
            client: self.client,
        }
    }
}

enum RefKind {
    Docking,
    Floating,
    Last,
}

fn xml_attr<'a>(attrs: &'a [OwnedAttribute], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| a.name.local_name == key)
        .map(|a| a.value.as_str())
}

fn bundled_path(mode: LayoutMode) -> PathBuf {
    crate::studio_assets_root()
        .join("layout")
        .join(format!("{}.xml", mode.file_stem()))
}
