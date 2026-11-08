//! UI Automation for a window whose controls are drawn: screen readers, and the PowerShell checks
//! `CLAUDE.md` describes, see buttons, rows, tabs, check boxes and drop-downs by name.
//!
//! After every frame, `snapshot` turns the element tree into an `AccessTree`: elements with a role,
//! every interactive one, and static text outside interactive ones, nested as they are on screen
//! (plain layout boxes are skipped). The window answers `WM_GETOBJECT` with a fragment root
//! (`Root`) whose children are `Node` fragments, looked up by element id in the latest snapshot,
//! so they never hold a stale element.
//!
//! Patterns: Invoke (buttons), SelectionItem (rows, nav items, tabs), Toggle (check boxes,
//! switches), ExpandCollapse and Value (drop-downs), Value (text boxes). Acting on one posts
//! `WM_UIA_ACTION` to the window, which handles it on the UI thread as a click would be. A text
//! box's Value reads and writes its EDIT control: setting it is like typing, since the EDIT's
//! `EN_CHANGE` reaches the app. (Hosting the EDIT's own provider under a node doesn't merge: hosts
//! apply only to fragment roots.)
//!
//! Providers use COM threading: UI Automation calls them on the UI thread.

#![allow(
    non_upper_case_globals,
    reason = "Windows' own names for the property, pattern and direction ids, matched as patterns"
)]

use std::sync::{Arc, Mutex};
use windows::Win32::{
    ExpandCollapseState, ExpandCollapseState_Collapsed, ExpandCollapseState_Expanded,
    ExpandCollapseState_LeafNode, HWND, IExpandCollapseProvider, IExpandCollapseProvider_Impl,
    IInvokeProvider, IInvokeProvider_Impl, IRawElementProviderFragment,
    IRawElementProviderFragment_Impl, IRawElementProviderFragmentRoot,
    IRawElementProviderFragmentRoot_Impl, IRawElementProviderSimple,
    IRawElementProviderSimple_Impl, ISelectionItemProvider, ISelectionItemProvider_Impl,
    IToggleProvider, IToggleProvider_Impl, IValueProvider, IValueProvider_Impl, NavigateDirection,
    NavigateDirection_FirstChild, NavigateDirection_LastChild, NavigateDirection_NextSibling,
    NavigateDirection_Parent, NavigateDirection_PreviousSibling, PATTERNID, PROPERTYID,
    PostMessageW, ProviderOptions, ProviderOptions_ServerSideProvider,
    ProviderOptions_UseComThreading, SAFEARRAY, SafeArrayCreateVector, SafeArrayPutElement,
    SetWindowTextW, ToggleState, ToggleState_Off, ToggleState_On,
    UIA_AutomationFocusChangedEventId, UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId,
    UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId, UIA_EditControlTypeId,
    UIA_ExpandCollapsePatternId, UIA_GroupControlTypeId, UIA_HasKeyboardFocusPropertyId,
    UIA_HelpTextPropertyId, UIA_InvokePatternId, UIA_IsContentElementPropertyId,
    UIA_IsControlElementPropertyId, UIA_IsEnabledPropertyId, UIA_IsKeyboardFocusablePropertyId,
    UIA_IsOffscreenPropertyId, UIA_ListItemControlTypeId, UIA_NamePropertyId,
    UIA_SelectionItemPatternId, UIA_TabItemControlTypeId, UIA_TextControlTypeId,
    UIA_TogglePatternId, UIA_ValuePatternId, UiaAppendRuntimeId, UiaHostProviderFromHwnd,
    UiaRaiseAutomationEvent, UiaRect, UiaReturnRawElementProvider, UiaRootObjectId, VARENUM,
    VARIANT, VT_BOOL, VT_BSTR, VT_I4, WM_APP, tagVARIANT_0, tagVARIANT_0_0, tagVARIANT_0_0_0,
};
use windows::core::{
    BOOL, BSTR, ComObjectInterface, Error, HRESULT, IUnknown, Interface, PCWSTR, Result, implement,
};

/// A VARIANT holding `value` as `vt` (the raw union, built without unsafe).
fn variant(vt: VARENUM, value: tagVARIANT_0_0_0) -> VARIANT {
    VARIANT {
        Anonymous: tagVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(tagVARIANT_0_0 {
                vt: vt as u16,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: value,
            }),
        },
    }
}

fn variant_i32(value: i32) -> VARIANT {
    variant(VT_I4, tagVARIANT_0_0_0 { lVal: value })
}

fn variant_bool(value: bool) -> VARIANT {
    // VARIANT_TRUE is -1.
    variant(
        VT_BOOL,
        tagVARIANT_0_0_0 {
            boolVal: if value { -1 } else { 0 },
        },
    )
}

/// UI Automation takes the string with the VARIANT and frees it.
fn variant_str(value: &str) -> VARIANT {
    variant(
        VT_BSTR,
        tagVARIANT_0_0_0 {
            bstrVal: std::mem::ManuallyDrop::new(BSTR::from(value)),
        },
    )
}

use super::el::{El, Font, Kind, Rect, Role};
use super::input::Frame;
use super::layout::Placed;

/// Posted to the window when UI Automation acts on an element: `wparam` is an `Action`,
/// `lparam` the element's id.
pub const WM_UIA_ACTION: i32 = WM_APP + 2;

/// What UI Automation asked an element to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Invoke = 0,
    Focus = 1,
    Expand = 2,
    Collapse = 3,
}

impl Action {
    pub fn from_wparam(value: usize) -> Option<Self> {
        [Self::Invoke, Self::Focus, Self::Expand, Self::Collapse]
            .into_iter()
            .find(|action| *action as usize == value)
    }
}

/// UIA_E_ELEMENTNOTAVAILABLE: the element is gone from the window.
const ELEMENT_NOT_AVAILABLE: HRESULT = HRESULT(0x8004_0201_u32 as i32);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccessNode {
    pub id: u64,
    pub parent: usize,
    pub children: Vec<usize>,
    pub role: Role,
    pub name: String,
    pub help: String,
    /// Client DIPs.
    pub rect: Rect,
    pub offscreen: bool,
    pub enabled: bool,
    pub focusable: bool,
    pub selected: bool,
    /// A drop-down's picked item.
    pub value: Option<String>,
    pub expanded: bool,
}

/// The accessible elements of the last frame. Node 0 is the window itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccessTree {
    pub nodes: Vec<AccessNode>,
    pub focus: Option<u64>,
    /// Pixels per DIP, and the client area's origin on the screen, in pixels.
    pub scale: f32,
    pub origin: (f32, f32),
    /// The EDIT controls' windows, by element id.
    pub edits: Vec<(u64, isize)>,
}

impl AccessTree {
    fn index(&self, id: u64) -> Option<usize> {
        self.nodes.iter().position(|node| node.id == id && id != 0)
    }

    fn screen_rect(&self, rect: Rect) -> UiaRect {
        UiaRect {
            left: f64::from(self.origin.0 + rect.x * self.scale),
            top: f64::from(self.origin.1 + rect.y * self.scale),
            width: f64::from(rect.w * self.scale),
            height: f64::from(rect.h * self.scale),
        }
    }
}

/// The accessible elements of `frame`, nested as on screen.
pub fn snapshot<M: Clone>(
    frame: &Frame<M>,
    focus: Option<u64>,
    open_dropdown: Option<u64>,
) -> AccessTree {
    let mut tree = AccessTree {
        nodes: vec![AccessNode {
            role: Role::Group,
            enabled: true,
            ..AccessNode::default()
        }],
        focus,
        scale: 1.0,
        origin: (0.0, 0.0),
        edits: Vec::new(),
    };
    walk(
        &frame.root,
        &frame.placed,
        0,
        false,
        open_dropdown,
        &mut tree,
    );
    tree
}

fn walk<M>(
    el: &El<M>,
    placed: &Placed,
    parent: usize,
    inside_control: bool,
    open: Option<u64>,
    tree: &mut AccessTree,
) {
    let interactive = el.is_interactive();
    let is_text = matches!(&el.kind, Kind::Text(spec) if spec.font != Font::Icons && !spec.text.trim().is_empty());
    let accessible = el.role != Role::None || interactive || (is_text && !inside_control);
    let mut here = parent;
    if accessible {
        let role = match el.role {
            Role::None if interactive => Role::Button,
            Role::None => Role::Text,
            role => role,
        };
        let value = match &el.kind {
            Kind::DropDown {
                items, selected, ..
            } => selected.and_then(|i| items.get(i)).cloned(),
            _ => None,
        };
        tree.nodes.push(AccessNode {
            id: placed.id,
            parent,
            children: Vec::new(),
            role,
            // A group (a card) is named only when given a name: its first text belongs to the
            // control inside it, which a name search should find instead.
            name: if role == Role::Group {
                el.name.clone().unwrap_or_default()
            } else {
                el.accessible_name()
            },
            help: el.tooltip.clone().unwrap_or_default(),
            rect: placed.rect,
            offscreen: placed.clip.is_empty(),
            enabled: el.enabled,
            focusable: interactive,
            selected: el.selected,
            value,
            expanded: open == Some(placed.id),
        });
        here = tree.nodes.len() - 1;
        tree.nodes[parent].children.push(here);
    }
    for (child, child_placed) in el.children.iter().zip(&placed.children) {
        walk(
            child,
            child_placed,
            here,
            inside_control || interactive,
            open,
            tree,
        );
    }
}

struct Shared {
    hwnd: isize,
    tree: Mutex<AccessTree>,
}

impl Shared {
    fn with<R>(&self, f: impl FnOnce(&AccessTree) -> R) -> R {
        match self.tree.lock() {
            Ok(tree) => f(&tree),
            Err(poisoned) => f(&poisoned.into_inner()),
        }
    }

    fn post(&self, action: Action, id: u64) {
        // SAFETY: posting to the window this provider belongs to; a window that is gone makes
        // the call fail, which is ignored.
        let _ = unsafe {
            PostMessageW(
                Some(self.hwnd as HWND),
                WM_UIA_ACTION as u32,
                action as usize,
                id as isize,
            )
        };
    }

    fn fragment(self: &Arc<Self>, index: usize) -> IRawElementProviderFragment {
        if index == 0 {
            Root {
                shared: Arc::clone(self),
            }
            .into()
        } else {
            let id = self.with(|tree| tree.nodes[index].id);
            Node {
                shared: Arc::clone(self),
                id,
            }
            .into()
        }
    }
}

/// The window's UI Automation provider.
pub struct Uia {
    shared: Arc<Shared>,
    root: IRawElementProviderSimple,
}

impl Uia {
    pub fn new(hwnd: isize) -> Self {
        let shared = Arc::new(Shared {
            hwnd,
            tree: Mutex::new(AccessTree::default()),
        });
        let root = Root {
            shared: Arc::clone(&shared),
        }
        .into();
        Self { shared, root }
    }

    /// Replaces the snapshot; raises a focus event when the keyboard focus moved.
    pub fn update(&self, tree: AccessTree) {
        let moved = {
            let mut current = match self.shared.tree.lock() {
                Ok(current) => current,
                Err(poisoned) => poisoned.into_inner(),
            };
            let moved = current.focus != tree.focus;
            *current = tree;
            moved.then_some(current.focus).flatten()
        };
        if let Some(id) = moved {
            let node: IRawElementProviderSimple = Node {
                shared: Arc::clone(&self.shared),
                id,
            }
            .into();
            // SAFETY: `node` is a live provider; a failure only means nobody listens.
            let _ = unsafe { UiaRaiseAutomationEvent(&node, UIA_AutomationFocusChangedEventId) };
        }
    }

    /// Answers `WM_GETOBJECT` for the window's root, else `None` (default processing).
    pub fn get_object(&self, hwnd: isize, wparam: usize, lparam: isize) -> Option<isize> {
        if lparam as i32 != UiaRootObjectId {
            return None;
        }
        // SAFETY: the window's own handle and the message's parameters, with a live provider.
        let result =
            unsafe { UiaReturnRawElementProvider(hwnd as HWND, wparam, lparam, &self.root) };
        Some(result)
    }
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot
)]
struct Root {
    shared: Arc<Shared>,
}

impl IRawElementProviderSimple_Impl for Root_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }

    fn GetPatternProvider(&self, _pattern: PATTERNID) -> Result<IUnknown> {
        Err(Error::empty())
    }

    fn GetPropertyValue(&self, _property: PROPERTYID) -> Result<VARIANT> {
        // The window's own name and type come from its host provider.
        Ok(VARIANT::default())
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        host_of(self.shared.hwnd)
    }
}

impl IRawElementProviderFragment_Impl for Root_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let child = self.shared.with(|tree| match direction {
            NavigateDirection_FirstChild => tree.nodes[0].children.first().copied(),
            NavigateDirection_LastChild => tree.nodes[0].children.last().copied(),
            _ => None,
        });
        child
            .map(|index| self.shared.fragment(index))
            .ok_or_else(Error::empty)
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        // The host provider gives the window's.
        Ok(std::ptr::null_mut())
    }

    fn get_BoundingRectangle(&self) -> Result<UiaRect> {
        // The host provider gives the window's.
        Ok(UiaRect::default())
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> HRESULT {
        HRESULT(0)
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(Root {
            shared: Arc::clone(&self.shared),
        }
        .into())
    }
}

impl IRawElementProviderFragmentRoot_Impl for Root_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let found = self.shared.with(|tree| {
            let px = (x as f32 - tree.origin.0) / tree.scale;
            let py = (y as f32 - tree.origin.1) / tree.scale;
            // The last node containing the point is the innermost and topmost.
            tree.nodes
                .iter()
                .enumerate()
                .skip(1)
                .rev()
                .find(|(_, node)| !node.offscreen && node.rect.contains(px, py))
                .map(|(index, _)| index)
        });
        Ok(self.shared.fragment(found.unwrap_or(0)))
    }

    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        let focus = self
            .shared
            .with(|tree| tree.focus.and_then(|id| tree.index(id)));
        focus
            .map(|index| self.shared.fragment(index))
            .ok_or_else(Error::empty)
    }
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IInvokeProvider,
    ISelectionItemProvider,
    IToggleProvider,
    IExpandCollapseProvider,
    IValueProvider
)]
struct Node {
    shared: Arc<Shared>,
    id: u64,
}

impl Node_Impl {
    fn node(&self) -> Result<AccessNode> {
        self.shared
            .with(|tree| tree.index(self.id).map(|index| tree.nodes[index].clone()))
            .ok_or_else(|| Error::from_hresult(ELEMENT_NOT_AVAILABLE))
    }
}

fn host_of(hwnd: isize) -> Result<IRawElementProviderSimple> {
    let mut host = None;
    // SAFETY: `hwnd` is a window of this process and `host` a valid out-pointer.
    unsafe { UiaHostProviderFromHwnd(hwnd as HWND, &mut host) }.ok()?;
    host.ok_or_else(Error::empty)
}

impl IRawElementProviderSimple_Impl for Node_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }

    fn GetPatternProvider(&self, pattern: PATTERNID) -> Result<IUnknown> {
        let node = self.node()?;
        let supported = match node.role {
            Role::Button => pattern == UIA_InvokePatternId,
            Role::ListItem | Role::Tab => pattern == UIA_SelectionItemPatternId,
            Role::CheckBox => pattern == UIA_TogglePatternId,
            Role::ComboBox => {
                pattern == UIA_ExpandCollapsePatternId || pattern == UIA_ValuePatternId
            }
            Role::Edit => pattern == UIA_ValuePatternId,
            _ => false,
        };
        if !supported {
            return Err(Error::empty());
        }
        let unknown: IUnknown = match pattern {
            UIA_InvokePatternId => {
                ComObjectInterface::<IInvokeProvider>::as_interface_ref(self).cast()?
            }
            UIA_SelectionItemPatternId => {
                ComObjectInterface::<ISelectionItemProvider>::as_interface_ref(self).cast()?
            }
            UIA_TogglePatternId => {
                ComObjectInterface::<IToggleProvider>::as_interface_ref(self).cast()?
            }
            UIA_ExpandCollapsePatternId => {
                ComObjectInterface::<IExpandCollapseProvider>::as_interface_ref(self).cast()?
            }
            _ => ComObjectInterface::<IValueProvider>::as_interface_ref(self).cast()?,
        };
        Ok(unknown)
    }

    fn GetPropertyValue(&self, property: PROPERTYID) -> Result<VARIANT> {
        let node = self.node()?;
        let focus = self.shared.with(|tree| tree.focus);
        Ok(match property {
            UIA_ControlTypePropertyId => variant_i32(match node.role {
                Role::Button => UIA_ButtonControlTypeId,
                Role::ListItem => UIA_ListItemControlTypeId,
                Role::Tab => UIA_TabItemControlTypeId,
                Role::CheckBox => UIA_CheckBoxControlTypeId,
                Role::ComboBox => UIA_ComboBoxControlTypeId,
                Role::Edit => UIA_EditControlTypeId,
                Role::Group => UIA_GroupControlTypeId,
                Role::Text | Role::None => UIA_TextControlTypeId,
            }),
            UIA_NamePropertyId => variant_str(&node.name),
            UIA_HelpTextPropertyId if !node.help.is_empty() => variant_str(&node.help),
            UIA_IsEnabledPropertyId => variant_bool(node.enabled),
            UIA_IsKeyboardFocusablePropertyId => variant_bool(node.focusable && node.enabled),
            UIA_HasKeyboardFocusPropertyId => variant_bool(focus == Some(node.id)),
            UIA_IsOffscreenPropertyId => variant_bool(node.offscreen),
            UIA_IsControlElementPropertyId | UIA_IsContentElementPropertyId => variant_bool(true),
            _ => VARIANT::default(),
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        // Only the fragment root has a host (the window).
        Err(Error::empty())
    }
}

impl Node_Impl {
    /// The EDIT control of a text box node.
    fn edit_window(&self) -> Option<isize> {
        self.shared.with(|tree| {
            tree.edits
                .iter()
                .find(|(id, _)| *id == self.id)
                .map(|(_, hwnd)| *hwnd)
        })
    }
}

impl IRawElementProviderFragment_Impl for Node_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let target = self.shared.with(|tree| {
            let index = tree.index(self.id)?;
            let node = &tree.nodes[index];
            let siblings = &tree.nodes[node.parent].children;
            let at = siblings.iter().position(|i| *i == index)?;
            match direction {
                NavigateDirection_Parent => Some(node.parent),
                NavigateDirection_FirstChild => node.children.first().copied(),
                NavigateDirection_LastChild => node.children.last().copied(),
                NavigateDirection_NextSibling => siblings.get(at + 1).copied(),
                NavigateDirection_PreviousSibling => at.checked_sub(1).map(|i| siblings[i]),
                _ => None,
            }
        });
        target
            .map(|index| self.shared.fragment(index))
            .ok_or_else(Error::empty)
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        let parts = [UiaAppendRuntimeId, self.id as i32, (self.id >> 32) as i32];
        // SAFETY: a one-dimensional array of 3 four-byte integers; UI Automation frees it.
        let array = unsafe { SafeArrayCreateVector(VT_I4 as u16, 0, parts.len() as u32) };
        if array.is_null() {
            return Err(Error::from_hresult(HRESULT(0x8007_000E_u32 as i32)));
        }
        for (index, part) in parts.iter().enumerate() {
            let index = index as i32;
            // SAFETY: `index` is inside the array just created, which holds 4-byte integers.
            unsafe { SafeArrayPutElement(array, &index, std::ptr::from_ref(part).cast()) }.ok()?;
        }
        Ok(array)
    }

    fn get_BoundingRectangle(&self) -> Result<UiaRect> {
        let node = self.node()?;
        if node.offscreen {
            return Ok(UiaRect::default());
        }
        Ok(self.shared.with(|tree| tree.screen_rect(node.rect)))
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> HRESULT {
        self.shared.post(Action::Focus, self.id);
        HRESULT(0)
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(Root {
            shared: Arc::clone(&self.shared),
        }
        .into())
    }
}

impl IInvokeProvider_Impl for Node_Impl {
    fn Invoke(&self) -> HRESULT {
        match self.node() {
            Ok(node) if node.enabled => {
                self.shared.post(Action::Invoke, self.id);
                HRESULT(0)
            }
            Ok(_) => HRESULT(0x8004_0200_u32 as i32),
            Err(error) => error.code(),
        }
    }
}

impl ISelectionItemProvider_Impl for Node_Impl {
    fn Select(&self) -> HRESULT {
        IInvokeProvider_Impl::Invoke(self)
    }
    fn AddToSelection(&self) -> HRESULT {
        IInvokeProvider_Impl::Invoke(self)
    }
    fn RemoveFromSelection(&self) -> HRESULT {
        HRESULT(0)
    }
    fn IsSelected(&self) -> Result<BOOL> {
        Ok(self.node()?.selected.into())
    }
    fn SelectionContainer(&self) -> Result<IRawElementProviderSimple> {
        Err(Error::empty())
    }
}

impl IToggleProvider_Impl for Node_Impl {
    fn Toggle(&self) -> HRESULT {
        IInvokeProvider_Impl::Invoke(self)
    }
    fn ToggleState(&self) -> Result<ToggleState> {
        Ok(if self.node()?.selected {
            ToggleState_On
        } else {
            ToggleState_Off
        })
    }
}

impl IExpandCollapseProvider_Impl for Node_Impl {
    fn Expand(&self) -> HRESULT {
        self.shared.post(Action::Expand, self.id);
        HRESULT(0)
    }
    fn Collapse(&self) -> HRESULT {
        self.shared.post(Action::Collapse, self.id);
        HRESULT(0)
    }
    fn ExpandCollapseState(&self) -> Result<ExpandCollapseState> {
        let node = self.node()?;
        Ok(match node.role {
            Role::ComboBox if node.expanded => ExpandCollapseState_Expanded,
            Role::ComboBox => ExpandCollapseState_Collapsed,
            _ => ExpandCollapseState_LeafNode,
        })
    }
}

/// UIA_E_INVALIDOPERATION.
const INVALID_OPERATION: HRESULT = HRESULT(0x8013_1509_u32 as i32);

impl IValueProvider_Impl for Node_Impl {
    /// A text box takes the text as if typed (its `EN_CHANGE` reaches the app); a drop-down's
    /// item is picked through ExpandCollapse instead.
    fn SetValue(&self, value: &PCWSTR) -> HRESULT {
        let editable = self
            .node()
            .is_ok_and(|node| node.role == Role::Edit && node.enabled);
        match self.edit_window() {
            Some(hwnd) if editable => {
                // SAFETY: UI Automation passes a valid wide string; the EDIT copies it.
                let set = unsafe { SetWindowTextW(hwnd as HWND, *value) };
                if set.as_bool() {
                    HRESULT(0)
                } else {
                    INVALID_OPERATION
                }
            }
            _ => INVALID_OPERATION,
        }
    }
    fn Value(&self) -> Result<BSTR> {
        let node = self.node()?;
        let value = match self.edit_window() {
            Some(hwnd) if node.role == Role::Edit => crate::shell::edit_box::text_of(hwnd),
            _ => node.value.unwrap_or_default(),
        };
        Ok(BSTR::from(value.as_str()))
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        let node = self.node()?;
        Ok((node.role != Role::Edit || !node.enabled).into())
    }
}
