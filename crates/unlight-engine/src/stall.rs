//! Submits work long enough to make the kernel reset the GPU, to prove a hang on the export
//! device doesn't take the editor down. It can disrupt other apps, so it never runs by itself.

use crate::{error::EngineError, gpu::GpuContext};
use std::{sync::Arc, time::Duration};

const WORKGROUPS: u32 = 1 << 20;

#[derive(Debug)]
pub struct StallOutcome {
    pub stalled_device_lost: bool,
    pub editor_device_lost: bool,
    pub elapsed: Duration,
}

/// Hangs `victim`, then reports whether `bystander` survived.
pub fn stall_device(victim: &Arc<GpuContext>, bystander: &Arc<GpuContext>) -> StallOutcome {
    let device = &victim.device;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("stall"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/stall.wgsl").into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("stall"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let sink = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("stall sink"),
        size: 4096,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: sink.as_entire_binding() }],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(WORKGROUPS, 1, 1);
    }
    let start = std::time::Instant::now();
    victim.queue.submit([encoder.finish()]);
    let result = victim.wait();
    StallOutcome {
        stalled_device_lost: matches!(result, Err(EngineError::DeviceLost(_))),
        editor_device_lost: bystander.is_lost(),
        elapsed: start.elapsed(),
    }
}
