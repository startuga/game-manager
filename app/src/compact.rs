//! Compact sizing for the whole app: every control about 24 DIP high, with tighter padding.
//!
//! - **WinUI's part**: the WinUI Gallery's Compact sizing sample loads
//!   `ms-appx:///Microsoft.UI.Xaml/DensityStyles/Compact.xaml` into a page's resources (text
//!   boxes, combo boxes, list rows). Game Manager has no XAML page, so the same dictionary is
//!   merged into the application's resources, after the reactor's `XamlControlsResources`, whose
//!   sizes it overrides. This also reaches dialogs and ComboBox drop-downs, which are outside the
//!   window's tree.
//! - **The rest** (`compact_controls.xaml`): buttons, check boxes, toggle switches and expanders,
//!   which Compact.xaml leaves out. WinUI's styles read their sizes as StaticResources, which a
//!   later dictionary cannot override by key, so it holds styles based on WinUI's.
//!   - It replaces `DefaultButtonStyle` (dialogs' buttons), `AccentButtonStyle` (the reactor's
//!     `ButtonStyle::Accent`, and a dialog's default button) and `SubtleButtonStyle`, which are
//!     looked up by key.
//!   - **Aliases**: WinUI's base styles are first added back as `WinUI<key>` (`BASES`), and the
//!     compact ones are based on those, since a style based on its own key could find itself.
//!   - **DropDownButton and Expander** have no keyed style in the application's resources (no
//!     `DefaultDropDownButtonStyle` in the runtime, whatever WinUI's source says). Their look is
//!     their default style (`DefaultStyleKey`), which WinUI applies under any implicit style, so
//!     their compact styles need no base.
//! - **What stays standard**: the check box's 20 px square, the switch's 40×20 track and the
//!   expander's chevron, which are fixed in WinUI's templates.
//! - **Measured** at 225% (physical px, standard → compact): buttons 74 → 56 (text boxes 55),
//!   expander headers 108 → 76, check boxes 72 → 54, switches 91 → 67, dialog buttons (the
//!   default one too) 71 → 53.
//! - **Why the app does it itself**: windows-reactor has no API for application resources, so this
//!   goes through `winui_bindings.rs`, generated for these calls.
//! - **When**: `GameManager::create`, which runs after the reactor's `OnLaunched` merged
//!   `XamlControlsResources` and before the first render, so every control is built with Compact
//!   sizes.

use crate::winui_bindings::{Application, PropertyValue, ResourceDictionary, Uri, XamlReader};
use windows_collections::IMap;
use windows_core::{IInspectable, Interface};

const COMPACT: &str = "ms-appx:///Microsoft.UI.Xaml/DensityStyles/Compact.xaml";
/// Buttons, check boxes, toggle switches and expanders.
const CONTROLS: &str = include_str!("compact_controls.xaml");
/// WinUI's styles that `compact_controls.xaml` is based on, which it finds as `WinUI<key>`.
const BASES: [&str; 4] = [
    "DefaultButtonStyle",
    "AccentButtonStyle",
    "SubtleButtonStyle",
    "DefaultCheckBoxStyle",
];

/// Merges the compact sizes into the application's resources. On the UI thread, once. When the
/// controls' part fails, WinUI's part stays.
pub(crate) fn use_compact_sizing() -> Result<(), String> {
    let resources = Application::Current()
        .and_then(|application| application.Resources())
        .map_err(|e| e.to_string())?;
    merge_winui(&resources).map_err(|e| format!("WinUI's Compact.xaml: {e}"))?;
    merge_controls(&resources)
        .map_err(|e| format!("buttons, check boxes, switches and expanders: {e}"))
}

/// WinUI's Compact.xaml.
fn merge_winui(resources: &ResourceDictionary) -> windows_core::Result<()> {
    let compact = ResourceDictionary::new()?;
    compact.SetSource(&Uri::CreateUri(COMPACT)?)?;
    resources.MergedDictionaries()?.Append(&compact)
}

/// WinUI's base styles under their aliases, then `compact_controls.xaml`.
fn merge_controls(resources: &ResourceDictionary) -> windows_core::Result<()> {
    let found = resources.cast::<IMap<IInspectable, IInspectable>>()?;
    let originals = ResourceDictionary::new()?;
    let aliases = originals.cast::<IMap<IInspectable, IInspectable>>()?;
    for key in BASES {
        let style = found.Lookup(&PropertyValue::CreateString(key)?)?;
        aliases.Insert(
            &PropertyValue::CreateString(&format!("WinUI{key}"))?,
            &style,
        )?;
    }
    let merged = resources.MergedDictionaries()?;
    merged.Append(&originals)?;
    let controls = XamlReader::Load(CONTROLS)?.cast::<ResourceDictionary>()?;
    merged.Append(&controls)
}
