use crate::gpu::GpuContext;
use std::sync::mpsc;

const QUERY_CAPACITY: u32 = 64;

/// Named GPU durations for one frame, in milliseconds.
#[derive(Clone, Debug, Default)]
pub struct FrameTimings {
    pub passes: Vec<(&'static str, f64)>,
}

impl FrameTimings {
    pub fn total_ms(&self) -> f64 {
        self.passes.iter().map(|(_, ms)| ms).sum()
    }
}

/// Timestamp queries around compute passes.
pub struct GpuProfiler {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    labels: Vec<&'static str>,
    period_ns: f64,
}

impl GpuProfiler {
    pub fn new(ctx: &GpuContext) -> Option<Self> {
        if !ctx.timestamps {
            return None;
        }
        let queries = ctx.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("profiler"),
            ty: wgpu::QueryType::Timestamp,
            count: QUERY_CAPACITY,
        });
        let size = u64::from(QUERY_CAPACITY) * 8;
        let resolve = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("profiler resolve"),
            size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("profiler readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(Self {
            queries,
            resolve,
            readback,
            labels: Vec::new(),
            period_ns: f64::from(ctx.queue.get_timestamp_period()),
        })
    }

    /// Reserves a pair of timestamps for a pass. `None` once the frame's queries run out.
    pub fn scope(&mut self, label: &'static str) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        let index = self.labels.len() as u32;
        if (index + 1) * 2 > QUERY_CAPACITY {
            return None;
        }
        self.labels.push(label);
        Some(wgpu::ComputePassTimestampWrites {
            query_set: &self.queries,
            beginning_of_pass_write_index: Some(index * 2),
            end_of_pass_write_index: Some(index * 2 + 1),
        })
    }

    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        let count = self.labels.len() as u32 * 2;
        encoder.resolve_query_set(&self.queries, 0..count, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback, 0, u64::from(count) * 8);
    }

    /// Reads results back after the frame's submission. Clears the frame.
    pub fn collect(&mut self, ctx: &GpuContext) -> crate::Result<FrameTimings> {
        let count = self.labels.len() as u64 * 2;
        let slice = self.readback.slice(..count * 8);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        ctx.wait()?;
        let mapped = receiver.recv().ok().and_then(|result| result.ok()).is_some();
        let mut timings = FrameTimings::default();
        if let Some(view) = mapped.then(|| slice.get_mapped_range().ok()).flatten() {
            let stamps: &[u64] = bytemuck::cast_slice(&view);
            for (index, label) in self.labels.iter().enumerate() {
                let ticks = stamps[index * 2 + 1].wrapping_sub(stamps[index * 2]);
                timings.passes.push((label, ticks as f64 * self.period_ns / 1e6));
            }
            drop(view);
            self.readback.unmap();
        }
        self.labels.clear();
        Ok(timings)
    }
}
