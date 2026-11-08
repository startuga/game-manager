//! The window's toolkit, drawn with Direct2D: an element tree (`el`), its layout (`layout`),
//! drawing (`render`), pointer and keyboard (`input`), the UI Automation tree (`uia`), the design
//! tokens (`theme`), the controls built on them (`widgets`) and decoded pictures (`image`).
//!
//! It replaces WinUI 3: no XAML, no Windows App Runtime. Text boxes are real Win32 EDIT controls
//! placed by the layout (`shell::edit_box`); everything else is drawn here.

pub mod el;
pub mod image;
pub mod input;
pub mod layout;
pub mod render;
pub mod theme;
pub mod uia;
pub mod widgets;
