use crate::error::{EngineError, Result};
use std::{
    env,
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}},
    time::Duration,
};

/// Longer than the kernel's own GPU timeout, so a hang surfaces as a lost device.
const WAIT_TIMEOUT: Duration = Duration::from_secs(15);

/// One wgpu device. Export uses its own context so a hung tile can't take down the editor.
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    pub timestamps: bool,
    lost: Arc<AtomicBool>,
    lost_reason: Arc<Mutex<Option<String>>>,
    first_error: Arc<Mutex<Option<String>>>,
}

fn adapter_rank(info: &wgpu::AdapterInfo) -> u8 {
    match info.device_type {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu | wgpu::DeviceType::Other => 2,
        wgpu::DeviceType::Cpu => 3,
    }
}

impl GpuContext {
    /// Opens the best Vulkan adapter. `UNLIGHT_ADAPTER` picks one by name substring.
    pub fn new() -> Result<Arc<Self>> {
        pollster::block_on(Self::open())
    }

    async fn open() -> Result<Arc<Self>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let wanted = env::var("UNLIGHT_ADAPTER").ok().map(|name| name.to_lowercase());
        let adapter = instance
            .enumerate_adapters(wgpu::Backends::VULKAN)
            .await
            .into_iter()
            .filter(|adapter| {
                wanted.as_ref().is_none_or(|name| adapter.get_info().name.to_lowercase().contains(name))
            })
            .min_by_key(|adapter| adapter_rank(&adapter.get_info()))
            .ok_or(EngineError::NoAdapter)?;

        let available = adapter.features();
        if !available.contains(wgpu::Features::FLOAT32_FILTERABLE) {
            return Err(EngineError::MissingFeature("FLOAT32_FILTERABLE"));
        }
        let optional = wgpu::Features::TIMESTAMP_QUERY;
        let required_features = wgpu::Features::FLOAT32_FILTERABLE | (available & optional);

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("unlight"),
                required_features,
                required_limits: adapter.limits(),
                memory_hints: wgpu::MemoryHints::Performance,
                ..Default::default()
            })
            .await
            .map_err(|error| EngineError::DeviceRequest(error.to_string()))?;

        let lost = Arc::new(AtomicBool::new(false));
        let lost_reason = Arc::new(Mutex::new(None));
        let (flag, reason_slot) = (lost.clone(), lost_reason.clone());
        device.set_device_lost_callback(move |reason, message| {
            *reason_slot.lock().unwrap() = Some(format!("{reason:?}: {message}"));
            flag.store(true, Ordering::SeqCst);
        });

        let first_error = Arc::new(Mutex::new(None));
        let error_slot = first_error.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            error_slot.lock().unwrap().get_or_insert_with(|| error.to_string());
        }));

        Ok(Arc::new(Self {
            instance,
            info: adapter.get_info(),
            adapter,
            timestamps: device.features().contains(wgpu::Features::TIMESTAMP_QUERY),
            device,
            queue,
            lost,
            lost_reason,
            first_error,
        }))
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    pub fn is_software(&self) -> bool {
        self.info.device_type == wgpu::DeviceType::Cpu
    }

    /// Reports any failure so far: a lost device first, then the first validation error.
    pub fn check(&self) -> Result<()> {
        if self.is_lost() {
            let reason = self.lost_reason.lock().unwrap().clone();
            return Err(EngineError::DeviceLost(reason.unwrap_or_else(|| "unknown".into())));
        }
        match self.first_error.lock().unwrap().take() {
            Some(error) => Err(EngineError::Gpu(error)),
            None => Ok(()),
        }
    }

    /// Blocks until submitted work finishes.
    pub fn wait(&self) -> Result<()> {
        let poll = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(WAIT_TIMEOUT),
        });
        self.check()?;
        poll.map(|_| ()).map_err(|error| EngineError::DeviceLost(error.to_string()))
    }

    /// Destroys the device, which is what a real GPU reset looks like from here.
    pub fn simulate_device_loss(&self) {
        self.device.destroy();
        let _ = self.device.poll(wgpu::PollType::Poll);
    }
}
