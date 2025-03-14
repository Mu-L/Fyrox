// Copyright (c) 2019-present Dmitry Stepanov and Fyrox Engine contributors.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! Path editor is a simple widget that has a text box, that shows the current path and a "..." button, that opens a file
//! selector. See [`PathEditor`] docs for more info and usage examples.

#![warn(missing_docs)]

use crate::{
    button::{ButtonBuilder, ButtonMessage},
    core::{pool::Handle, reflect::prelude::*, type_traits::prelude::*, visitor::prelude::*},
    define_constructor,
    file_browser::{FileSelectorBuilder, FileSelectorMessage},
    grid::{Column, GridBuilder, Row},
    message::{MessageDirection, UiMessage},
    text::TextMessage,
    text_box::TextBoxBuilder,
    widget::{Widget, WidgetBuilder, WidgetMessage},
    window::{WindowBuilder, WindowMessage, WindowTitle},
    BuildContext, Control, Thickness, UiNode, UserInterface,
};

use fyrox_core::uuid_provider;
use fyrox_core::variable::InheritableVariable;
use fyrox_graph::constructor::{ConstructorProvider, GraphNodeConstructor};
use std::{
    ops::{Deref, DerefMut},
    path::Path,
    path::PathBuf,
};

/// A set of messages for the [`PathEditor`] widget.
#[derive(Debug, Clone, PartialEq)]
pub enum PathEditorMessage {
    /// A message, that is used to set new value of the editor or to receive changes from the editor.
    Path(PathBuf),
}

impl PathEditorMessage {
    define_constructor!(
        /// Creates [`PathEditorMessage::Path`] message.
        PathEditorMessage:Path => fn path(PathBuf), layout: false
    );
}

/// Path editor is a simple widget that has a text box, that shows the current path and a "..." button, that opens a file
/// selector.
///
/// ## Examples
///
/// An instance of the editor could be created like so:
///
/// ```rust
/// # use fyrox_ui::{
/// #     core::pool::Handle, path::PathEditorBuilder, widget::WidgetBuilder, BuildContext, UiNode,
/// # };
/// # use std::path::PathBuf;
/// #
/// fn create_path_editor(path: PathBuf, ctx: &mut BuildContext) -> Handle<UiNode> {
///     PathEditorBuilder::new(WidgetBuilder::new())
///         .with_path(path)
///         .build(ctx)
/// }
/// ```
///
/// To receive the changes, listen to [`PathEditorMessage::Path`] and check for its direction, it should be [`MessageDirection::FromWidget`].
/// To set a new path value, send [`PathEditorMessage::Path`] message, but with [`MessageDirection::ToWidget`].
#[derive(Default, Clone, Visit, Reflect, Debug, ComponentProvider)]
#[reflect(derived_type = "UiNode")]
pub struct PathEditor {
    /// Base widget of the editor.
    pub widget: Widget,
    /// A handle of the text field, that is used to show current path.
    pub text_field: InheritableVariable<Handle<UiNode>>,
    /// A button, that opens a file selection.
    pub select: InheritableVariable<Handle<UiNode>>,
    /// Current file selector instance, could be [`Handle::NONE`] if the selector is closed.
    pub selector: InheritableVariable<Handle<UiNode>>,
    /// Current path.
    pub path: InheritableVariable<PathBuf>,
}

impl ConstructorProvider<UiNode, UserInterface> for PathEditor {
    fn constructor() -> GraphNodeConstructor<UiNode, UserInterface> {
        GraphNodeConstructor::new::<Self>()
            .with_variant("Path Editor", |ui| {
                PathEditorBuilder::new(WidgetBuilder::new().with_name("Path Editor"))
                    .build(&mut ui.build_ctx())
                    .into()
            })
            .with_group("Input")
    }
}

crate::define_widget_deref!(PathEditor);

uuid_provider!(PathEditor = "51cfe7ec-ec31-4354-9578-047004b213a1");

impl Control for PathEditor {
    fn handle_routed_message(&mut self, ui: &mut UserInterface, message: &mut UiMessage) {
        self.widget.handle_routed_message(ui, message);

        if let Some(ButtonMessage::Click) = message.data() {
            if message.destination() == *self.select {
                self.selector.set_value_and_mark_modified(
                    FileSelectorBuilder::new(
                        WindowBuilder::new(
                            WidgetBuilder::new().with_width(300.0).with_height(450.0),
                        )
                        .open(false)
                        .with_title(WindowTitle::text("Select a Path")),
                    )
                    .build(&mut ui.build_ctx()),
                );

                ui.send_message(FileSelectorMessage::path(
                    *self.selector,
                    MessageDirection::ToWidget,
                    (*self.path).clone(),
                ));
                ui.send_message(WindowMessage::open_modal(
                    *self.selector,
                    MessageDirection::ToWidget,
                    true,
                    true,
                ));
                ui.send_message(FileSelectorMessage::focus_current_path(
                    *self.selector,
                    MessageDirection::ToWidget,
                ));
            }
        } else if let Some(PathEditorMessage::Path(path)) = message.data() {
            if message.destination() == self.handle
                && message.direction() == MessageDirection::ToWidget
                && &*self.path != path
            {
                self.path.set_value_and_mark_modified(path.clone());

                ui.send_message(TextMessage::text(
                    *self.text_field,
                    MessageDirection::ToWidget,
                    path.to_string_lossy().to_string(),
                ));
                ui.send_message(message.reverse());
            }
        }
    }

    fn preview_message(&self, ui: &UserInterface, message: &mut UiMessage) {
        if let Some(FileSelectorMessage::Commit(path)) = message.data() {
            if message.destination() == *self.selector && &*self.path != path {
                ui.send_message(WidgetMessage::remove(
                    *self.selector,
                    MessageDirection::ToWidget,
                ));

                ui.send_message(PathEditorMessage::path(
                    self.handle,
                    MessageDirection::ToWidget,
                    path.clone(),
                ));
            }
        }
    }
}

/// Path editor builder creates [`PathEditor`] instances and adds them to the user interface.
pub struct PathEditorBuilder {
    widget_builder: WidgetBuilder,
    path: PathBuf,
}

impl PathEditorBuilder {
    /// Creates new builder instance.
    pub fn new(widget_builder: WidgetBuilder) -> Self {
        Self {
            widget_builder,
            path: Default::default(),
        }
    }

    /// Sets the desired path.
    pub fn with_path<P: AsRef<Path>>(mut self, path: P) -> Self {
        path.as_ref().clone_into(&mut self.path);
        self
    }

    /// Finishes widget building and adds it to the user interface returning a handle to the instance.
    pub fn build(self, ctx: &mut BuildContext) -> Handle<UiNode> {
        let text_field;
        let select;
        let grid = GridBuilder::new(
            WidgetBuilder::new()
                .with_child({
                    text_field = TextBoxBuilder::new(
                        WidgetBuilder::new()
                            .on_column(0)
                            .with_margin(Thickness::uniform(1.0)),
                    )
                    .with_text(self.path.to_string_lossy())
                    .with_editable(false)
                    .build(ctx);
                    text_field
                })
                .with_child({
                    select = ButtonBuilder::new(
                        WidgetBuilder::new()
                            .on_column(1)
                            .with_width(30.0)
                            .with_margin(Thickness::uniform(1.0)),
                    )
                    .with_text("...")
                    .build(ctx);
                    select
                }),
        )
        .add_row(Row::stretch())
        .add_column(Column::stretch())
        .add_column(Column::auto())
        .build(ctx);

        let canvas = PathEditor {
            widget: self
                .widget_builder
                .with_child(grid)
                .with_preview_messages(true)
                .build(ctx),
            text_field: text_field.into(),
            select: select.into(),
            selector: Default::default(),
            path: self.path.into(),
        };
        ctx.add_node(UiNode::new(canvas))
    }
}

#[cfg(test)]
mod test {
    use crate::path::PathEditorBuilder;
    use crate::{test::test_widget_deletion, widget::WidgetBuilder};

    #[test]
    fn test_deletion() {
        test_widget_deletion(|ctx| PathEditorBuilder::new(WidgetBuilder::new()).build(ctx));
    }
}
