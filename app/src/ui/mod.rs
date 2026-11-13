//! The window's toolkit, drawn with Direct2D: an element tree (`el`), its layout (`layout`),
//! drawing (`render`), pointer and keyboard (`input`), text boxes' editing (`text_edit`), the UI
//! Automation tree (`uia`), the design tokens (`theme`), the controls built on them (`widgets`)
//! and decoded pictures (`image`).
//!
//! It replaces WinUI 3: no XAML, no Windows App Runtime, and no GDI: everything, text boxes
//! included, is drawn here with Direct2D and DirectWrite. The window has no child windows.

pub mod el;
pub mod image;
pub mod input;
pub mod layout;
pub mod render;
pub mod text_edit;
pub mod theme;
pub mod uia;
pub mod widgets;
