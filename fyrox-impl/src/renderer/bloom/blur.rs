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

use crate::{
    core::{algebra::Vector2, math::Rect, sstorage::ImmutableString},
    renderer::{
        framework::{
            error::FrameworkError,
            framebuffer::{Attachment, AttachmentKind, FrameBuffer},
            geometry_buffer::GeometryBuffer,
            gpu_program::{GpuProgram, UniformLocation},
            gpu_texture::{
                Coordinate, GpuTexture, GpuTextureKind, MagnificationFilter, MinificationFilter,
                PixelKind, WrapMode,
            },
            state::PipelineState,
            DrawParameters, ElementRange,
        },
        make_viewport_matrix, RenderPassStatistics,
    },
};
use std::{cell::RefCell, rc::Rc};

struct Shader {
    program: GpuProgram,
    world_view_projection_matrix: UniformLocation,
    image: UniformLocation,
    pixel_size: UniformLocation,
    horizontal: UniformLocation,
}

impl Shader {
    fn new(state: &PipelineState) -> Result<Self, FrameworkError> {
        let fragment_source = include_str!("../shaders/gaussian_blur_fs.glsl");
        let vertex_source = include_str!("../shaders/flat_vs.glsl");

        let program =
            GpuProgram::from_source(state, "GaussianBlurShader", vertex_source, fragment_source)?;
        Ok(Self {
            world_view_projection_matrix: program
                .uniform_location(state, &ImmutableString::new("worldViewProjection"))?,
            image: program.uniform_location(state, &ImmutableString::new("image"))?,
            pixel_size: program.uniform_location(state, &ImmutableString::new("pixelSize"))?,
            horizontal: program.uniform_location(state, &ImmutableString::new("horizontal"))?,
            program,
        })
    }
}

pub struct GaussianBlur {
    shader: Shader,
    h_framebuffer: FrameBuffer,
    v_framebuffer: FrameBuffer,
    width: usize,
    height: usize,
}

fn create_framebuffer(
    state: &PipelineState,
    width: usize,
    height: usize,
    pixel_kind: PixelKind,
) -> Result<FrameBuffer, FrameworkError> {
    let frame = {
        let kind = GpuTextureKind::Rectangle { width, height };
        let mut texture = GpuTexture::new(
            state,
            kind,
            pixel_kind,
            MinificationFilter::Nearest,
            MagnificationFilter::Nearest,
            1,
            None,
        )?;
        texture
            .bind_mut(state, 0)
            .set_wrap(Coordinate::S, WrapMode::ClampToEdge)
            .set_wrap(Coordinate::T, WrapMode::ClampToEdge);
        texture
    };

    FrameBuffer::new(
        state,
        None,
        vec![Attachment {
            kind: AttachmentKind::Color,
            texture: Rc::new(RefCell::new(frame)),
        }],
    )
}

impl GaussianBlur {
    pub fn new(
        state: &PipelineState,
        width: usize,
        height: usize,
        pixel_kind: PixelKind,
    ) -> Result<Self, FrameworkError> {
        Ok(Self {
            shader: Shader::new(state)?,
            h_framebuffer: create_framebuffer(state, width, height, pixel_kind)?,
            v_framebuffer: create_framebuffer(state, width, height, pixel_kind)?,
            width,
            height,
        })
    }

    fn h_blurred(&self) -> Rc<RefCell<GpuTexture>> {
        self.h_framebuffer.color_attachments()[0].texture.clone()
    }

    pub fn result(&self) -> Rc<RefCell<GpuTexture>> {
        self.v_framebuffer.color_attachments()[0].texture.clone()
    }

    pub(crate) fn render(
        &mut self,
        state: &PipelineState,
        quad: &GeometryBuffer,
        input: Rc<RefCell<GpuTexture>>,
    ) -> Result<RenderPassStatistics, FrameworkError> {
        let mut stats = RenderPassStatistics::default();

        let viewport = Rect::new(0, 0, self.width as i32, self.height as i32);

        let inv_size = Vector2::new(1.0 / self.width as f32, 1.0 / self.height as f32);
        let shader = &self.shader;

        // Blur horizontally first.
        stats += self.h_framebuffer.draw(
            quad,
            state,
            viewport,
            &shader.program,
            &DrawParameters {
                cull_face: None,
                color_write: Default::default(),
                depth_write: false,
                stencil_test: None,
                depth_test: false,
                blend: None,
                stencil_op: Default::default(),
            },
            ElementRange::Full,
            |mut program_binding| {
                program_binding
                    .set_matrix4(
                        &shader.world_view_projection_matrix,
                        &(make_viewport_matrix(viewport)),
                    )
                    .set_vector2(&shader.pixel_size, &inv_size)
                    .set_bool(&shader.horizontal, true)
                    .set_texture(&shader.image, &input);
            },
        )?;

        // Then blur vertically.
        let h_blurred_texture = self.h_blurred();
        stats += self.v_framebuffer.draw(
            quad,
            state,
            viewport,
            &shader.program,
            &DrawParameters {
                cull_face: None,
                color_write: Default::default(),
                depth_write: false,
                stencil_test: None,
                depth_test: false,
                blend: None,
                stencil_op: Default::default(),
            },
            ElementRange::Full,
            |mut program_binding| {
                program_binding
                    .set_matrix4(
                        &shader.world_view_projection_matrix,
                        &(make_viewport_matrix(viewport)),
                    )
                    .set_vector2(&shader.pixel_size, &inv_size)
                    .set_bool(&shader.horizontal, false)
                    .set_texture(&shader.image, &h_blurred_texture);
            },
        )?;

        Ok(stats)
    }
}
