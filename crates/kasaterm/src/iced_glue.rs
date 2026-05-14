//! iced 0.14 integration — opt-in via the `iced` feature.
//!
//! kasaterm itself stays UI-framework-agnostic. This module wires
//! `TerminalPipeline` / `TerminalPrimitive` into iced's Shader widget so
//! a Rust app using iced for chrome can drop a `Shader::new(primitive)`
//! widget into its tree and have kasaterm render the terminal body.

use iced::Rectangle;
use iced::widget::shader;
use iced::wgpu;

use crate::{Rect, TerminalPipeline, TerminalPrimitive};

impl shader::Primitive for TerminalPrimitive {
    type Pipeline = TerminalPipeline;

    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &shader::Viewport,
    ) {
        // iced hands us logical bounds (1×) but the render pass writes
        // to a physical surface at scale_factor × pixels per logical px.
        // Pass scale through so kasaterm bakes glyphs at the physical
        // size; placement divides back to logical for iced's projection.
        let scale = viewport.scale_factor() as f32;
        pipeline.prepare(
            device,
            queue,
            Rect {
                x: bounds.x,
                y: bounds.y,
                width: bounds.width,
                height: bounds.height,
            },
            self,
            scale,
        );
    }

    fn draw(
        &self,
        pipeline: &Self::Pipeline,
        render_pass: &mut wgpu::RenderPass<'_>,
    ) -> bool {
        pipeline.draw(render_pass);
        true
    }
}

impl shader::Pipeline for TerminalPipeline {
    fn new(
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Self {
        TerminalPipeline::new(device, format)
    }
}
