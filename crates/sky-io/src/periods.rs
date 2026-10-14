// SPDX-License-Identifier: GPL-3.0-or-later
//! What buffer sizes an output device can run at, as Windows reports them.
//! A buffer size request the device cannot meet is quietly rounded up, so
//! checks use this to explain why a request was not honored.

/// The buffer periods an output device supports, in frames at its mix rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DevicePeriods {
    pub sample_rate: u32,
    /// The period of the shared-mode audio engine by default.
    pub shared_default: u32,
    /// The smallest period a low-latency shared-mode stream can ask for
    /// (Windows 10 and later, `IAudioClient3`), and the step above it.
    pub shared_min: u32,
    pub shared_step: u32,
    /// The smallest period in exclusive mode.
    pub exclusive_min: u32,
}

/// The periods of the output device whose name contains `name`, or of the
/// default output device. `None` if the system cannot tell (or off Windows).
pub fn device_periods(name: Option<&str>) -> Option<DevicePeriods> {
    #[cfg(windows)]
    {
        let name = name.map(str::to_lowercase);
        // A thread of its own, so COM can be set up here whatever the
        // calling thread already uses.
        std::thread::spawn(move || {
            // SAFETY: COM calls on a fresh thread that initializes and
            // uninitializes COM itself; every pointer comes from COM.
            unsafe { wasapi::periods(name.as_deref()) }
        })
        .join()
        .ok()
        .flatten()
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        None
    }
}

#[cfg(windows)]
mod wasapi {
    use windows::Win32::Devices::Properties::DEVPKEY_Device_FriendlyName;
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::{
        DEVICE_STATE_ACTIVE, IAudioClient3, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
        eConsole, eRender,
    };
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize, STGM_READ,
    };

    use super::DevicePeriods;

    unsafe fn friendly_name(device: &IMMDevice) -> Option<String> {
        unsafe {
            let store = device.OpenPropertyStore(STGM_READ).ok()?;
            let key = &DEVPKEY_Device_FriendlyName as *const _ as *const PROPERTYKEY;
            let value = store.GetValue(key).ok()?;
            let text = PropVariantToStringAlloc(&value).ok()?;
            let name = text.to_string().ok();
            CoTaskMemFree(Some(text.0 as *const _));
            name
        }
    }

    unsafe fn find(name: Option<&str>) -> Option<IMMDevice> {
        unsafe {
            let devices: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let Some(name) = name else {
                return devices.GetDefaultAudioEndpoint(eRender, eConsole).ok();
            };
            let all = devices
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .ok()?;
            (0..all.GetCount().ok()?)
                .filter_map(|i| all.Item(i).ok())
                .find(|d| friendly_name(d).is_some_and(|n| n.to_lowercase().contains(name)))
        }
    }

    unsafe fn query(name: Option<&str>) -> Option<DevicePeriods> {
        unsafe {
            let device = find(name)?;
            let client: IAudioClient3 = device.Activate(CLSCTX_ALL, None).ok()?;
            let format = client.GetMixFormat().ok()?;
            let sample_rate = std::ptr::addr_of!((*format).nSamplesPerSec).read_unaligned();
            let (mut default, mut step, mut min, mut max) = (0, 0, 0, 0);
            let shared = client.GetSharedModeEnginePeriod(
                format,
                &mut default,
                &mut step,
                &mut min,
                &mut max,
            );
            CoTaskMemFree(Some(format as *const _));
            shared.ok()?;
            let mut exclusive = 0i64;
            client.GetDevicePeriod(None, Some(&mut exclusive)).ok()?;
            // 100-nanosecond units to frames.
            let exclusive_min =
                (exclusive as f64 / 10_000_000.0 * sample_rate as f64).round() as u32;
            Some(DevicePeriods {
                sample_rate,
                shared_default: default,
                shared_min: min,
                shared_step: step,
                exclusive_min,
            })
        }
    }

    pub(super) unsafe fn periods(name: Option<&str>) -> Option<DevicePeriods> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().ok()?;
            let found = query(name);
            CoUninitialize();
            found
        }
    }
}
