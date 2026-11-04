//! WinUI's Compact sizing for the whole app: shorter text boxes, combo boxes and list rows, with
//! tighter padding (buttons, check boxes, toggle switches and expanders keep their size).
//!
//! - **As WinUI does it**: the WinUI Gallery's Compact sizing sample loads
//!   `ms-appx:///Microsoft.UI.Xaml/DensityStyles/Compact.xaml` into a page's resources. Game
//!   Manager has no XAML page, so the same dictionary is merged into the application's resources,
//!   after the reactor's `XamlControlsResources`, whose sizes it overrides. This also reaches
//!   dialogs and ComboBox drop-downs, which are outside the window's tree.
//! - **Why the app does it itself**: windows-reactor has no API for application resources, so this
//!   goes through `winui_bindings.rs`, generated for these calls.
//! - **When**: `GameManager::create`, which runs after the reactor's `OnLaunched` merged
//!   `XamlControlsResources` and before the first render, so every control is built with Compact
//!   sizes.

use crate::winui_bindings::{Application, ResourceDictionary, Uri};

const COMPACT: &str = "ms-appx:///Microsoft.UI.Xaml/DensityStyles/Compact.xaml";

/// Merges WinUI's Compact sizes into the application's resources. On the UI thread, once.
pub(crate) fn use_compact_sizing() -> windows_core::Result<()> {
    let compact = ResourceDictionary::new()?;
    compact.SetSource(&Uri::CreateUri(COMPACT)?)?;
    Application::Current()?
        .Resources()?
        .MergedDictionaries()?
        .Append(&compact)
}
