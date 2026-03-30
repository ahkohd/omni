use std::sync::Arc;

use anyhow::{Context, Result};
use gpui::{
    App, Bounds, CustomBindingDesc, CustomBindingKind, CustomBindingName, CustomBindingValue,
    CustomBufferDesc, CustomBufferId, CustomBufferSource, CustomDrawParams, CustomPipelineDesc,
    CustomPipelineId, CustomPipelineState, CustomPrimitiveTopology, CustomVertexAttribute,
    CustomVertexAttributeName, CustomVertexBuffer, CustomVertexFetch, CustomVertexFormat,
    CustomVertexLayout, Pixels, Size, Window, canvas, div, prelude::*,
};

// Aurora shader tuning (experiment knobs)
pub const AURORA_IDLE_OPACITY: f32 = 0.55;
pub const AURORA_SPEECH_OPACITY_BOOST: f32 = 0.35;
pub const AURORA_IDLE_FLOW_SPEED: f32 = 0.25;
pub const AURORA_FLOW_SPEED_BOOST: f32 = 0.50;
pub const AURORA_BASE_DISTORTION: f32 = 0.0;
pub const AURORA_DISTORTION_BOOST: f32 = 0.0;
pub const AURORA_RMS_GAIN: f32 = 24.0;
pub const AURORA_PEAK_GAIN: f32 = 7.0;

const AURORA_SHADER_SOURCE: &str = include_str!("shaders/aurora.wgsl");

#[derive(Debug, Default)]
pub struct AuroraRenderer {
    pipeline: Option<CustomPipelineId>,
    vertex_buffer: Option<CustomBufferId>,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct AuroraUniformInput {
    pub time: f32,
    pub energy: f32,
    pub silent: bool,
    pub fill_state: f32,
}

impl AuroraRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ensure_resources(&mut self, window: &mut Window) {
        if self.pipeline.is_some() || self.error.is_some() {
            return;
        }

        match Self::build_resources(window) {
            Ok((pipeline, vertex_buffer)) => {
                self.pipeline = Some(pipeline);
                self.vertex_buffer = Some(vertex_buffer);
            }
            Err(error) => {
                // Graceful fallback: no shader overlay on unsupported platforms.
                self.error = Some(error.to_string());
            }
        }
    }

    pub fn render_overlay(&self, input: AuroraUniformInput) -> gpui::Div {
        let Some(pipeline) = self.pipeline else {
            return div();
        };
        let Some(vertex_buffer) = self.vertex_buffer else {
            return div();
        };

        let uniform = aurora_uniform_bytes(input);
        let prepaint = move |bounds: Bounds<_>, window: &mut Window, _cx: &mut App| {
            let vertex_data = quad_vertex_data_for_bounds(bounds, window.viewport_size());
            let _ = window.update_custom_buffer(vertex_buffer, Arc::clone(&vertex_data));

            CustomDrawParams {
                bounds,
                pipeline,
                vertex_buffers: vec![CustomVertexBuffer {
                    source: CustomBufferSource::Buffer(vertex_buffer),
                }],
                vertex_count: 6,
                index_buffer: None,
                index_count: 0,
                target: None,
                instance_count: 1,
                push_constants: None,
                bindings: vec![CustomBindingValue::Uniform(CustomBufferSource::Inline(
                    Arc::clone(&uniform),
                ))],
            }
        };

        let paint = move |_bounds: Bounds<_>,
                          params: CustomDrawParams,
                          window: &mut Window,
                          _cx: &mut App| {
            let _ = window.paint_custom(params);
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .overflow_hidden()
            .child(canvas(prepaint, paint).size_full())
    }

    fn build_resources(window: &mut Window) -> Result<(CustomPipelineId, CustomBufferId)> {
        let pipeline = window
            .create_custom_pipeline(CustomPipelineDesc {
                name: "omni_transcript_aurora".to_string(),
                shader_source: AURORA_SHADER_SOURCE.to_string(),
                vertex_entry: "vs_main".to_string(),
                fragment_entry: "fs_main".to_string(),
                vertex_fetches: vec![CustomVertexFetch {
                    layout: CustomVertexLayout {
                        stride: 16,
                        attributes: vec![
                            CustomVertexAttribute {
                                name: CustomVertexAttributeName::A0,
                                offset: 0,
                                format: CustomVertexFormat::F32Vec2,
                                location: None,
                            },
                            CustomVertexAttribute {
                                name: CustomVertexAttributeName::A1,
                                offset: 8,
                                format: CustomVertexFormat::F32Vec2,
                                location: None,
                            },
                        ],
                    },
                    instanced: false,
                }],
                primitive: CustomPrimitiveTopology::TriangleList,
                color_targets: Vec::new(),
                state: CustomPipelineState::default(),
                push_constants: None,
                bindings: vec![CustomBindingDesc {
                    name: CustomBindingName::B0,
                    kind: CustomBindingKind::Uniform { size: 32 },
                    slot: None,
                }],
            })
            .context("failed creating aurora shader pipeline")?;

        let vertex_buffer = window
            .create_custom_buffer(CustomBufferDesc {
                name: "omni_transcript_aurora_quad".to_string(),
                data: quad_vertex_data(),
            })
            .context("failed creating aurora vertex buffer")?;

        Ok((pipeline, vertex_buffer))
    }
}

fn aurora_uniform_bytes(input: AuroraUniformInput) -> Arc<[u8]> {
    let mut data = Vec::with_capacity(32);
    let activity = if input.silent { 0.0 } else { 1.0 };
    let speed = AURORA_IDLE_FLOW_SPEED + input.energy * AURORA_FLOW_SPEED_BOOST;
    let distortion = AURORA_BASE_DISTORTION + input.energy * AURORA_DISTORTION_BOOST;
    let opacity = AURORA_IDLE_OPACITY + input.energy * AURORA_SPEECH_OPACITY_BOOST;

    push_f32(&mut data, input.time);
    push_f32(&mut data, input.energy);
    push_f32(&mut data, opacity);
    push_f32(&mut data, distortion);
    push_f32(&mut data, speed);
    push_f32(&mut data, activity);
    push_f32(&mut data, input.fill_state);
    push_f32(&mut data, 0.0);
    Arc::from(data)
}

fn quad_vertex_data() -> Arc<[u8]> {
    let mut data = Vec::with_capacity(6 * 4 * 4);
    let vertices = [
        (-1.0f32, 1.0f32, 0.0f32, 0.0f32),
        (1.0f32, 1.0f32, 1.0f32, 0.0f32),
        (1.0f32, -1.0f32, 1.0f32, 1.0f32),
        (-1.0f32, 1.0f32, 0.0f32, 0.0f32),
        (1.0f32, -1.0f32, 1.0f32, 1.0f32),
        (-1.0f32, -1.0f32, 0.0f32, 1.0f32),
    ];

    for (x, y, u, v) in vertices {
        push_f32(&mut data, x);
        push_f32(&mut data, y);
        push_f32(&mut data, u);
        push_f32(&mut data, v);
    }

    Arc::from(data)
}

fn quad_vertex_data_for_bounds(bounds: Bounds<Pixels>, viewport: Size<Pixels>) -> Arc<[u8]> {
    let mut data = Vec::with_capacity(6 * 4 * 4);

    let viewport_w = f32::from(viewport.width).max(1.0);
    let viewport_h = f32::from(viewport.height).max(1.0);

    let left_px = f32::from(bounds.origin.x);
    let top_px = f32::from(bounds.origin.y);
    let right_px = left_px + f32::from(bounds.size.width);
    let bottom_px = top_px + f32::from(bounds.size.height);

    let left_ndc = (left_px / viewport_w) * 2.0 - 1.0;
    let right_ndc = (right_px / viewport_w) * 2.0 - 1.0;
    let top_ndc = 1.0 - (top_px / viewport_h) * 2.0;
    let bottom_ndc = 1.0 - (bottom_px / viewport_h) * 2.0;

    let vertices = [
        (left_ndc, top_ndc, 0.0f32, 0.0f32),
        (right_ndc, top_ndc, 1.0f32, 0.0f32),
        (right_ndc, bottom_ndc, 1.0f32, 1.0f32),
        (left_ndc, top_ndc, 0.0f32, 0.0f32),
        (right_ndc, bottom_ndc, 1.0f32, 1.0f32),
        (left_ndc, bottom_ndc, 0.0f32, 1.0f32),
    ];

    for (x, y, u, v) in vertices {
        push_f32(&mut data, x);
        push_f32(&mut data, y);
        push_f32(&mut data, u);
        push_f32(&mut data, v);
    }

    Arc::from(data)
}

fn push_f32(buffer: &mut Vec<u8>, value: f32) {
    buffer.extend_from_slice(&value.to_ne_bytes());
}
