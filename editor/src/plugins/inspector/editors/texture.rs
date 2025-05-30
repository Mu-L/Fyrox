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

use crate::asset::item::AssetItem;
use crate::fyrox::graph::BaseSceneGraph;
use crate::fyrox::{
    asset::manager::ResourceManager,
    asset::untyped::UntypedResource,
    core::{
        algebra::Vector2, make_relative_path, pool::Handle, reflect::prelude::*,
        type_traits::prelude::*, uuid_provider, visitor::prelude::*,
    },
    gui::{
        define_constructor,
        image::{ImageBuilder, ImageMessage},
        inspector::{
            editors::{
                PropertyEditorBuildContext, PropertyEditorDefinition, PropertyEditorInstance,
                PropertyEditorMessageContext, PropertyEditorTranslationContext,
            },
            FieldKind, InspectorError, PropertyChanged,
        },
        message::{MessageDirection, UiMessage},
        widget::{Widget, WidgetBuilder, WidgetMessage},
        BuildContext, Control, Thickness, UiNode, UserInterface,
    },
    resource::texture::{Texture, TextureResource},
};
use crate::plugins::inspector::EditorEnvironment;

use std::{
    any::TypeId,
    fmt::{Debug, Formatter},
    ops::{Deref, DerefMut},
};

#[derive(Clone, Visit, Reflect, ComponentProvider)]
#[reflect(derived_type = "UiNode")]
pub struct TextureEditor {
    widget: Widget,
    image: Handle<UiNode>,
    #[visit(skip)]
    #[reflect(hidden)]
    resource_manager: ResourceManager,
    texture: Option<TextureResource>,
}

impl Debug for TextureEditor {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "TextureEditor")
    }
}

impl Deref for TextureEditor {
    type Target = Widget;

    fn deref(&self) -> &Self::Target {
        &self.widget
    }
}

impl DerefMut for TextureEditor {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.widget
    }
}

#[derive(Debug, PartialEq, Clone, Eq)]
pub enum TextureEditorMessage {
    Texture(Option<TextureResource>),
}

impl TextureEditorMessage {
    define_constructor!(TextureEditorMessage:Texture => fn texture(Option<TextureResource>), layout: false);
}

uuid_provider!(TextureEditor = "5db49479-ff89-49b8-a038-0766253d6493");

impl Control for TextureEditor {
    fn handle_routed_message(&mut self, ui: &mut UserInterface, message: &mut UiMessage) {
        self.widget.handle_routed_message(ui, message);

        if let Some(WidgetMessage::Drop(dropped)) = message.data::<WidgetMessage>() {
            if message.destination() == self.image {
                if let Some(item) = ui.node(*dropped).cast::<AssetItem>() {
                    if let Ok(relative_path) = make_relative_path(&item.path) {
                        ui.send_message(TextureEditorMessage::texture(
                            self.handle(),
                            MessageDirection::ToWidget,
                            self.resource_manager.try_request::<Texture>(relative_path),
                        ));
                    }
                }
            }
        } else if let Some(TextureEditorMessage::Texture(texture)) =
            message.data::<TextureEditorMessage>()
        {
            if &self.texture != texture && message.direction() == MessageDirection::ToWidget {
                self.texture.clone_from(texture);

                ui.send_message(ImageMessage::texture(
                    self.image,
                    MessageDirection::ToWidget,
                    self.texture.clone(),
                ));

                ui.send_message(message.reverse());
            }
        }
    }
}

pub struct TextureEditorBuilder {
    widget_builder: WidgetBuilder,
    texture: Option<TextureResource>,
}

impl TextureEditorBuilder {
    pub fn new(widget_builder: WidgetBuilder) -> Self {
        Self {
            widget_builder,
            texture: None,
        }
    }

    pub fn with_texture(mut self, texture: Option<TextureResource>) -> Self {
        self.texture = texture;
        self
    }

    pub fn build(
        self,
        ctx: &mut BuildContext,
        resource_manager: ResourceManager,
    ) -> Handle<UiNode> {
        let image;
        let widget = self
            .widget_builder
            .with_child({
                image = ImageBuilder::new(
                    WidgetBuilder::new()
                        .with_margin(Thickness::uniform(1.0))
                        .with_allow_drop(true),
                )
                .with_checkerboard_background(true)
                .with_opt_texture(self.texture)
                .build(ctx);
                image
            })
            .build(ctx);

        let editor = TextureEditor {
            widget,
            image,
            resource_manager,
            texture: None,
        };

        ctx.add_node(UiNode::new(editor))
    }
}

#[derive(Debug)]
pub struct TexturePropertyEditorDefinition {
    pub untyped: bool,
}

impl TexturePropertyEditorDefinition {
    fn value(&self, field_info: &FieldRef) -> Result<Option<TextureResource>, InspectorError> {
        if self.untyped {
            let value = field_info.cast_value::<Option<UntypedResource>>()?;
            let casted = value.as_ref().and_then(|r| r.try_cast::<Texture>());
            Ok(casted)
        } else {
            Ok(field_info.cast_value::<Option<TextureResource>>()?.clone())
        }
    }
}

impl PropertyEditorDefinition for TexturePropertyEditorDefinition {
    fn value_type_id(&self) -> TypeId {
        if self.untyped {
            TypeId::of::<Option<UntypedResource>>()
        } else {
            TypeId::of::<Option<TextureResource>>()
        }
    }

    fn create_instance(
        &self,
        ctx: PropertyEditorBuildContext,
    ) -> Result<PropertyEditorInstance, InspectorError> {
        let value = self.value(ctx.property_info)?;
        let environment = EditorEnvironment::try_get_from(&ctx.environment)?;

        Ok(PropertyEditorInstance::Simple {
            editor: TextureEditorBuilder::new(
                WidgetBuilder::new().with_min_size(Vector2::new(0.0, 17.0)),
            )
            .with_texture(value.clone())
            .build(ctx.build_context, environment.resource_manager.clone()),
        })
    }

    fn create_message(
        &self,
        ctx: PropertyEditorMessageContext,
    ) -> Result<Option<UiMessage>, InspectorError> {
        let value = self.value(ctx.property_info)?;

        Ok(Some(TextureEditorMessage::texture(
            ctx.instance,
            MessageDirection::ToWidget,
            value.clone(),
        )))
    }

    fn translate_message(&self, ctx: PropertyEditorTranslationContext) -> Option<PropertyChanged> {
        if ctx.message.direction() == MessageDirection::FromWidget {
            if let Some(TextureEditorMessage::Texture(value)) =
                ctx.message.data::<TextureEditorMessage>()
            {
                return Some(PropertyChanged {
                    name: ctx.name.to_string(),
                    value: if self.untyped {
                        FieldKind::object(value.clone().map(|r| r.into_untyped()))
                    } else {
                        FieldKind::object(value.clone())
                    },
                });
            }
        }
        None
    }
}
