//! Compact sizing for the whole app: every control about 24 DIP high, with tighter padding. One
//! dictionary built in Rust from the tables below, merged into the application's resources: no
//! XAML document is loaded or parsed, and no string is built at run time.
//!
//! - **WinUI's part** (`COMPACT` and the TextBlock style): the values of WinUI's
//!   `DensityStyles/Compact.xaml`, the dictionary its Gallery's Compact sizing sample loads (text
//!   boxes, combo boxes, list rows, …). They are copied (WinUI is MIT): a WinUI that changes them
//!   is not followed. Its ComboBox sizes are replaced by this app's.
//! - **The rest**: buttons, menu items, check boxes, toggle switches and expanders, which
//!   Compact.xaml leaves out, and combo boxes and their items, which it leaves taller than its
//!   text boxes.
//!   - **ThemeResource keys** (`SIZES`) are overridden by key, as Compact.xaml does:
//!     `ButtonPadding` (HyperlinkButton reads it), the ComboBox and ToggleSwitch sizes.
//!   - **StaticResources**: WinUI's styles read most sizes as StaticResources, which a later
//!     dictionary cannot override by key, so `FIRST_FRAME` and `LATER` hold styles based on
//!     WinUI's own, found by key in the application's resources before their dictionary is
//!     merged. Among them are `DefaultButtonStyle` (dialogs' buttons), `AccentButtonStyle` (the
//!     reactor's `ButtonStyle::Accent`, and a dialog's default button) and `SubtleButtonStyle`.
//!   - **DropDownButton and Expander** have no keyed style in the application's resources (no
//!     `DefaultDropDownButtonStyle` in the runtime, whatever WinUI's source says). Their look is
//!     their default style (`DefaultStyleKey`), which WinUI applies under any implicit style, so
//!     their compact styles need no base.
//! - **Values**: doubles are boxed with `PropertyValue`. A `Thickness` is given as its text
//!   (`"8,1,8,2"`) and boxed by WinUI itself (`XamlBindingHelper::ConvertValue`): WinUI read a
//!   `Thickness` boxed by windows-reference as 0 (measured: no padding at all), although the box
//!   has the same interface and class name as its own.
//! - **No string allocation**: keys, type names and thicknesses are `h!` literals, UTF-16
//!   HSTRINGs stored in the binary, passed by reference (`winui_bindings.rs` is not minimal, so it
//!   takes `&HSTRING`).
//!   - WinRT still owns what it keeps: a boxed key copies its string, and so does a type name
//!     (`TypeName` holds an owned HSTRING).
//!   - Strings are formatted only for an error's message.
//! - **What stays standard**: these are fixed in WinUI's templates.
//!   - Radio buttons stay 32 high: their circle sits in a 32 px grid. A smaller padding would only
//!     lift their text off the circle.
//!   - The check box's 20 px square, the switch's 40×20 track and the expander's chevron.
//! - **Measured** at 225% (physical px, standard → compact):
//!   - Buttons 74 → 56, text boxes 73 → 55, combo boxes 90 → 56.
//!   - Menu items 80 → 53, expander headers 108 → 76.
//!   - Check boxes 72 → 54, switches 91 → 67.
//!   - Dialog buttons, the default one too, 71 → 53.
//! - **Time** (release build, median of 6 starts):
//!   - 2.0 ms before the first frame, then 1.6 ms after it. Everything at once took 3.2 ms, and
//!     3.8 ms when Compact.xaml and a XAML dictionary were parsed.
//!   - Most of it is WinUI realizing its own styles at their first lookup, which a first render
//!     or a first panel would otherwise do: DefaultButtonStyle about 0.9 ms, DefaultCheckBoxStyle
//!     0.9, DefaultMenuFlyoutItemStyle 0.3. This module's own work is about 0.4 ms.
//! - **Why the app does it itself**: windows-reactor has no API for application resources, so this
//!   goes through `winui_bindings.rs`, generated for these calls.
//! - **When**: in two dictionaries.
//!   - `use_compact_sizing` (the sizes and `FIRST_FRAME`) from `GameManager::create`. It runs
//!     after the reactor's `OnLaunched` merged `XamlControlsResources` and before the first
//!     render, so every control of the first frame is built with Compact sizes.
//!   - `use_later_styles` (`LATER`: check boxes, menu items, Subtle buttons) once the first frame
//!     is drawn, `LATER_DELAY` after `create`. WinUI looks a control's implicit style up when the
//!     control is created, so the panels and menus that show them later are compact too.

use crate::winui_bindings::{
    Application, Control, DependencyProperty, FrameworkElement, PropertyValue, ResourceDictionary,
    Setter, SetterBase, Style, TextBlock, TypeKind, TypeName, XamlBindingHelper,
};
use windows_collections::{IMap, IVector};
use windows_core::{HSTRING, IInspectable, Interface, h};
use windows_reference::IReference;

/// Compact.xaml's sizes, less `ComboBoxMinHeight` and `ComboBoxPadding` (in `SIZES`).
static COMPACT: [(&HSTRING, Size); 14] = [
    (h!("ControlContentThemeFontSize"), Size::Double(14.0)),
    (h!("ContentControlFontSize"), Size::Double(14.0)),
    (h!("TextControlThemeMinHeight"), Size::Double(24.0)),
    (
        h!("TextControlThemePadding"),
        Size::Thickness(h!("2,2,6,1")),
    ),
    (h!("ListViewItemMinHeight"), Size::Double(32.0)),
    (h!("TreeViewItemMinHeight"), Size::Double(24.0)),
    (
        h!("TreeViewItemMultiSelectCheckBoxMinHeight"),
        Size::Double(24.0),
    ),
    (h!("TreeViewItemPresenterMargin"), Size::Double(0.0)),
    (h!("TreeViewItemPresenterPadding"), Size::Double(0.0)),
    (h!("TimePickerHostPadding"), Size::Thickness(h!("0,1,0,2"))),
    (h!("DatePickerHostPadding"), Size::Thickness(h!("0,1,0,2"))),
    (
        h!("DatePickerHostMonthPadding"),
        Size::Thickness(h!("9,0,0,1")),
    ),
    (
        h!("ComboBoxEditableTextPadding"),
        Size::Thickness(h!("10,0,30,0")),
    ),
    (h!("NavigationViewItemOnLeftMinHeight"), Size::Double(32.0)),
];

/// WinUI: 11,5,11,6 (32 high).
const BUTTON_PADDING: Size = Size::Thickness(h!("8,1,8,2"));

/// This app's ThemeResource sizes, with WinUI's own and Compact.xaml's values.
static SIZES: [(&HSTRING, Size); 6] = [
    (h!("ButtonPadding"), BUTTON_PADDING),
    // WinUI: 10 each. The switch's track keeps its own 0,5 margin, so 5 is the least.
    (h!("ToggleSwitchPreContentMargin"), Size::Double(5.0)),
    (h!("ToggleSwitchPostContentMargin"), Size::Double(5.0)),
    // The runtime's ComboBox is at least ComboBoxMinHeight + 8 high (Compact.xaml's 24 gave 32), so
    // the floor goes below the content and the padding sets the height, measured like the text
    // boxes. WinUI: 12,5,0,7 and 32; Compact.xaml: 12,1,0,3 and 24.
    (h!("ComboBoxPadding"), Size::Thickness(h!("12,-2,0,0"))),
    (h!("ComboBoxMinHeight"), Size::Double(16.0)),
    // The drop-down's items. WinUI: 11,5,11,7.
    (
        h!("ComboBoxItemThemePadding"),
        Size::Thickness(h!("11,1,11,3")),
    ),
];

const BUTTON: &HSTRING = h!("Microsoft.UI.Xaml.Controls.Button");
const BUTTON_SETTERS: &[(Property, Size)] = &[(Property::Padding, BUTTON_PADDING)];

/// The styles of controls the first frame shows: merged before it.
static FIRST_FRAME: [CompactStyle; 6] = [
    // Looked up by key: dialogs' buttons, and the reactor's ButtonStyle::Accent (Play).
    CompactStyle {
        key: Some(h!("DefaultButtonStyle")),
        target: BUTTON,
        based_on: Some(h!("DefaultButtonStyle")),
        setters: BUTTON_SETTERS,
    },
    CompactStyle {
        key: Some(h!("AccentButtonStyle")),
        target: BUTTON,
        based_on: Some(h!("AccentButtonStyle")),
        setters: BUTTON_SETTERS,
    },
    // Every control without a style of its own.
    CompactStyle {
        key: None,
        target: BUTTON,
        based_on: Some(h!("DefaultButtonStyle")),
        setters: BUTTON_SETTERS,
    },
    CompactStyle {
        key: None,
        target: h!("Microsoft.UI.Xaml.Controls.DropDownButton"),
        based_on: None,
        setters: BUTTON_SETTERS,
    },
    // WinUI: MinHeight 48 (the header's), Padding 16 (the content's).
    CompactStyle {
        key: None,
        target: h!("Microsoft.UI.Xaml.Controls.Expander"),
        based_on: None,
        setters: &[
            (Property::MinHeight, Size::Double(32.0)),
            (Property::Padding, Size::Thickness(h!("12"))),
        ],
    },
    // Compact.xaml's own.
    CompactStyle {
        key: None,
        target: h!("Microsoft.UI.Xaml.Controls.TextBlock"),
        based_on: None,
        setters: &[(Property::FontSize, Size::Double(14.0))],
    },
];

/// The styles of controls the first frame never shows, merged once it is drawn (`LATER_DELAY`):
/// WinUI realizes the styles they are based on at their first lookup, about 1.3 ms. A control
/// that the first frame shows must have its style in `FIRST_FRAME`: WinUI does not restyle a
/// control that already exists.
static LATER: [CompactStyle; 3] = [
    // The reactor's ButtonStyle::Subtle: the game menu and the ReShade panels.
    CompactStyle {
        key: Some(h!("SubtleButtonStyle")),
        target: BUTTON,
        based_on: Some(h!("SubtleButtonStyle")),
        setters: BUTTON_SETTERS,
    },
    // The items of menus (the ReShade button's). WinUI: 11,8,11,9.
    CompactStyle {
        key: None,
        target: h!("Microsoft.UI.Xaml.Controls.MenuFlyoutItem"),
        based_on: Some(h!("DefaultMenuFlyoutItemStyle")),
        setters: &[(Property::Padding, Size::Thickness(h!("11,2,11,3")))],
    },
    // ReShade ▸ Shaders. WinUI: MinHeight 32, Padding 8,5,0,0 (the text's top, beside the 20 px
    // box).
    CompactStyle {
        key: None,
        target: h!("Microsoft.UI.Xaml.Controls.CheckBox"),
        based_on: Some(h!("DefaultCheckBoxStyle")),
        setters: &[
            (Property::MinHeight, Size::Double(24.0)),
            (Property::Padding, Size::Thickness(h!("8,2,0,0"))),
        ],
    },
];

/// How long after `GameManager::create` the `LATER` styles are merged: past the first frame, which
/// WinUI draws within a frame or two of the window's start, and long before anyone can open a
/// menu or a panel.
pub(crate) const LATER_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// A style: its key (`None`: implicit, for every `target` without a style of its own), its type,
/// the key of WinUI's style it is based on, and what it sets.
struct CompactStyle {
    key: Option<&'static HSTRING>,
    /// The type's full name (`Microsoft.UI.Xaml.Controls.Button`).
    target: &'static HSTRING,
    based_on: Option<&'static HSTRING>,
    setters: &'static [(Property, Size)],
}

/// A property a compact style sets.
#[derive(Clone, Copy)]
enum Property {
    Padding,
    MinHeight,
    FontSize,
}

impl Property {
    fn get(self) -> windows_core::Result<DependencyProperty> {
        match self {
            Property::Padding => Control::PaddingProperty(),
            Property::MinHeight => FrameworkElement::MinHeightProperty(),
            Property::FontSize => TextBlock::FontSizeProperty(),
        }
    }
}

/// A size, as WinUI's resources and setters hold it.
#[derive(Clone, Copy)]
enum Size {
    Double(f64),
    /// As XAML writes it (`"8,1,8,2"`, or `"12"` for all four sides).
    Thickness(&'static HSTRING),
}

impl Size {
    /// The size as a WinRT object. `thickness` is the `Thickness` type, for WinUI to box one.
    fn boxed(self, thickness: &TypeName) -> windows_core::Result<IInspectable> {
        match self {
            Size::Double(value) => PropertyValue::CreateDouble(value),
            Size::Thickness(text) => {
                XamlBindingHelper::ConvertValue(thickness, &PropertyValue::CreateString(text)?)
            }
        }
    }
}

/// The type named `name`, with the one copy of its name (a TypeName owns it).
fn type_name(name: &HSTRING) -> TypeName {
    TypeName {
        Name: name.clone(),
        Kind: TypeKind::Metadata,
    }
}

/// Merges the sizes and the first frame's styles. On the UI thread, once, before the first render.
/// The error names what failed.
pub(crate) fn use_compact_sizing() -> Result<(), String> {
    merge(COMPACT.iter().chain(&SIZES), &FIRST_FRAME)
}

/// Merges the `LATER` styles. On the UI thread, once, after the first frame (`LATER_DELAY`).
pub(crate) fn use_later_styles() -> Result<(), String> {
    merge(std::iter::empty(), &LATER)
}

/// Builds a dictionary of `sizes` and `styles` and merges it into the application's resources,
/// last, so its keys win.
fn merge<'a>(
    sizes: impl Iterator<Item = &'a (&'static HSTRING, Size)>,
    styles: &[CompactStyle],
) -> Result<(), String> {
    let resources = Application::Current()
        .and_then(|application| application.Resources())
        .map_err(|e| e.to_string())?;
    // WinUI's own styles, which the compact ones are based on: found before this dictionary
    // replaces some of them.
    let found = resources
        .cast::<IMap<IInspectable, IInspectable>>()
        .map_err(|e| e.to_string())?;
    let compact = ResourceDictionary::new().map_err(|e| e.to_string())?;
    let entries = compact
        .cast::<IMap<IInspectable, IInspectable>>()
        .map_err(|e| e.to_string())?;
    let thickness = type_name(h!("Microsoft.UI.Xaml.Thickness"));
    for &(key, size) in sizes {
        add_size(&entries, key, size, &thickness).map_err(failed(key))?;
    }
    for style in styles {
        add_style(&entries, &found, style, &thickness)
            .map_err(failed(style.key.unwrap_or(style.target)))?;
    }
    resources
        .MergedDictionaries()
        .and_then(|merged| merged.Append(&compact))
        .map_err(|e| e.to_string())
}

/// An error's message, naming the key or type it was for.
fn failed(what: &HSTRING) -> impl FnOnce(windows_core::Error) -> String + '_ {
    move |e| format!("{}: {e}", what.to_string_lossy())
}

fn add_size(
    entries: &IMap<IInspectable, IInspectable>,
    key: &HSTRING,
    size: Size,
    thickness: &TypeName,
) -> windows_core::Result<()> {
    entries
        .Insert(&PropertyValue::CreateString(key)?, &size.boxed(thickness)?)
        .map(drop)
}

/// Builds `style` and adds it to `entries`, under its key or, for an implicit style, its type.
fn add_style(
    entries: &IMap<IInspectable, IInspectable>,
    found: &IMap<IInspectable, IInspectable>,
    style: &CompactStyle,
    thickness: &TypeName,
) -> windows_core::Result<()> {
    let target = type_name(style.target);
    let built = Style::new()?;
    built.SetTargetType(&target)?;
    if let Some(base) = style.based_on {
        let original = found
            .Lookup(&PropertyValue::CreateString(base)?)?
            .cast::<Style>()?;
        built.SetBasedOn(&original)?;
    }
    let setters = built.Setters()?.cast::<IVector<SetterBase>>()?;
    for &(property, size) in style.setters {
        let setter = Setter::new()?;
        setter.SetProperty(&property.get()?)?;
        setter.SetValue(&size.boxed(thickness)?)?;
        setters.Append(&setter.cast::<SetterBase>()?)?;
    }
    let key = match style.key {
        Some(key) => PropertyValue::CreateString(key)?,
        None => IReference::from(target).into(),
    };
    entries.Insert(&key, &built).map(drop)
}
