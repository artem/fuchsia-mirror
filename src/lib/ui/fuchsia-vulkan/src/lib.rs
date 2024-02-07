// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use derivative::Derivative;
use fuchsia_zircon::sys::zx_handle_t;
use std::{
    ffi::CStr,
    mem::{self, MaybeUninit},
    os::raw::c_char,
    ptr,
};
use vk_sys as vk;

macro_rules! ptrs {(
    $struct_name:ident,
    { $($name:ident => ($($param_n:ident: $param_ty:ty),*) -> $ret:ty,)+ }) => {
        #[allow(non_snake_case)]
        pub struct $struct_name {
            $(
                pub $name: extern "system" fn($($param_ty),*) -> $ret,
            )+
        }

        impl $struct_name {
            pub fn load<F>(mut f: F) -> $struct_name
                where F: FnMut(&CStr) -> *const ::std::ffi::c_void
            {
                #[allow(non_snake_case)]
                $struct_name {
                    $(
                        $name: unsafe {
                            extern "system" fn $name($(_: $param_ty),*) {
                                panic!("function pointer `{}` not loaded", stringify!($name))
                            }
                            let name = ::std::ffi::CStr::from_bytes_with_nul_unchecked(
                                concat!("vk", stringify!($name), "\0").as_bytes());
                            let val = f(name);
                            if val.is_null() {
                                ::std::mem::transmute($name as *const ())
                            } else {
                                ::std::mem::transmute(val)
                            }
                        },
                    )+
                }
            }
            $(
                /// # Safety
                ///
                #[doc = concat!("This wraps a direct call to `vk", stringify!($name), "`.")]
                /// The safety requirements of that function must be upheld when calling this one.
                #[doc = concat!(
                    "See the [extension function documentation](",
                    "https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vk",
                    stringify!($name),
                    ".html) for more details."
                )]
                #[inline]
                #[allow(non_snake_case)]
                pub unsafe fn $name(&self $(, $param_n: $param_ty)*) -> $ret {
                    let ptr = self.$name;
                    ptr($($param_n),*)
                }
            )+
        }

        impl ::std::fmt::Debug for $struct_name {
            fn fmt(&self, fmt: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                fmt.debug_struct(stringify!($struct_name)).finish()
            }
        }
    };
}

// TODO: Remove buffer collection bindings when they are upstream.
pub type BufferCollectionFUCHSIA = u64;

// VK_FUCHSIA_imagepipe_surface
pub const STRUCTURE_TYPE_IMAGEPIPE_SURFACE_CREATE_INFO_FUCHSIA: u32 = 1000214000;

// VK_FUCHSIA_external_memory
pub const STRUCTURE_TYPE_IMPORT_MEMORY_ZIRCON_HANDLE_INFO_FUCHSIA: u32 = 1000364000;
pub const STRUCTURE_TYPE_MEMORY_ZIRCON_HANDLE_PROPERTIES_FUCHSIA: u32 = 1000364001;
pub const STRUCTURE_TYPE_MEMORY_GET_ZIRCON_HANDLE_INFO_FUCHSIA: u32 = 1000364002;

// VK_FUCHSIA_external_semaphore
pub const STRUCTURE_TYPE_IMPORT_SEMAPHORE_ZIRCON_HANDLE_INFO_FUCHSIA: u32 = 1000365000;
pub const STRUCTURE_TYPE_SEMAPHORE_GET_ZIRCON_HANDLE_INFO_FUCHSIA: u32 = 1000365001;

// VK_FUCHSIA_buffer_collection
pub const STRUCTURE_TYPE_BUFFER_COLLECTION_CREATE_INFO_FUCHSIA: u32 = 1000366000;
pub const STRUCTURE_TYPE_IMPORT_MEMORY_BUFFER_COLLECTION_FUCHSIA: u32 = 1000366001;
pub const STRUCTURE_TYPE_BUFFER_COLLECTION_IMAGE_CREATE_INFO_FUCHSIA: u32 = 1000366002;
pub const STRUCTURE_TYPE_BUFFER_COLLECTION_PROPERTIES_FUCHSIA: u32 = 1000366003;
pub const STRUCTURE_TYPE_BUFFER_CONSTRAINTS_INFO_FUCHSIA: u32 = 1000366004;
pub const STRUCTURE_TYPE_BUFFER_COLLECTION_BUFFER_CREATE_INFO_FUCHSIA: u32 = 1000366005;
pub const STRUCTURE_TYPE_IMAGE_CONSTRAINTS_INFO_FUCHSIA: u32 = 1000366006;
pub const STRUCTURE_TYPE_IMAGE_FORMAT_CONSTRAINTS_INFO_FUCHSIA: u32 = 1000366007;
pub const STRUCTURE_TYPE_SYSMEM_COLOR_SPACE_FUCHSIA: u32 = 1000366008;
pub const STRUCTURE_TYPE_BUFFER_COLLECTION_CONSTRAINTS_INFO_FUCHSIA: u32 = 1000366009;

pub const IMAGE_CONSTRAINTS_INFO_CPU_READ_RARELY_FUCHSIA: u32 = 1;
pub const IMAGE_CONSTRAINTS_INFO_CPU_READ_OFTEN_FUCHSIA: u32 = 2;
pub const IMAGE_CONSTRAINTS_INFO_CPU_WRITE_RARELY_FUCHSIA: u32 = 4;
pub const IMAGE_CONSTRAINTS_INFO_CPU_WRITE_OFTEN_FUCHSIA: u32 = 8;
pub const IMAGE_CONSTRAINTS_INFO_PROTECTED_OPTIONAL_FUCHSIA: u32 = 16;

pub const EXTERNAL_SEMAPHORE_HANDLE_TYPE_ZIRCON_EVENT_BIT_FUCHSIA: u32 = 128;

pub const EXTERNAL_MEMORY_HANDLE_TYPE_ZIRCON_VMO_BIT_FUCHSIA: u32 = 2048;

pub const OBJECT_TYPE_BUFFER_COLLECTION_FUCHSIA: u32 = 1000366000;

pub type ImageFormatConstraintsFlagsFUCHSIA = vk::Flags;

pub type ImageConstraintsInfoFlagsFUCHSIA = vk::Flags;

// VK_FUCHSIA_imagepipe_surface
#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Debug, Default)]
pub struct ImagePipeSurfaceCreateInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_IMAGEPIPE_SURFACE_CREATE_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub flags: u32,
    pub imagePipeHandle: zx_handle_t,
}

// VK_FUCHSIA_external_memory

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Debug, Default)]
pub struct ImportMemoryZirconHandleInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_IMPORT_MEMORY_ZIRCON_HANDLE_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub handleType: u32,
    pub handle: zx_handle_t,
}

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Debug, Default)]
pub struct MemoryZirconHandlePropertiesFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_MEMORY_ZIRCON_HANDLE_PROPERTIES_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null_mut()"))]
    pub pNext: *mut ::std::os::raw::c_void,
    pub memoryTypeBits: u32,
}

// VK_FUCHSIA_external_semaphore

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Debug, Default)]
pub struct ImportSemaphoreZirconHandleInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_IMPORT_SEMAPHORE_ZIRCON_HANDLE_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub semaphore: vk::Semaphore,
    pub flags: u32,
    pub handleType: u32,
    pub zirconHandle: zx_handle_t,
}

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Debug, Default)]
pub struct SemaphoreGetZirconHandleInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_SEMAPHORE_GET_ZIRCON_HANDLE_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub semaphore: vk::Semaphore,
    pub handleType: u32,
}

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Debug, Default)]
pub struct MemoryGetZirconHandleInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_MEMORY_GET_ZIRCON_HANDLE_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub memory: vk::DeviceMemory,
    pub handleType: u32,
}

// VK_FUCHSIA_buffer_collection

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Debug, Default)]
pub struct BufferCollectionCreateInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_BUFFER_COLLECTION_CREATE_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub collectionToken: u32,
}

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Debug, Default)]
pub struct BufferCollectionImageCreateInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_BUFFER_COLLECTION_IMAGE_CREATE_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub collection: BufferCollectionFUCHSIA,
    pub index: u32,
}

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Debug, Default)]
pub struct BufferCollectionConstraintsInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_BUFFER_COLLECTION_CONSTRAINTS_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub minBufferCount: u32,
    pub maxBufferCount: u32,
    pub minBufferCountForCamping: u32,
    pub minBufferCountForDedicatedSlack: u32,
    pub minBufferCountForSharedSlack: u32,
}

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Default)]
pub struct BufferConstraintsInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_BUFFER_COLLECTION_CONSTRAINTS_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    #[derivative(Default(value = "unsafe { MaybeUninit::zeroed().assume_init() }"))]
    pub createInfo: vk::BufferCreateInfo,
    pub requiredFormatFeatures: vk::FormatFeatureFlags,
    pub bufferCollectionConstraints: BufferCollectionConstraintsInfoFUCHSIA,
}

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Default)]
pub struct BufferCollectionBufferCreateInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_BUFFER_COLLECTION_BUFFER_CREATE_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub collection: BufferCollectionFUCHSIA,
    pub index: u32,
}

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Debug, Default)]
pub struct SysmemColorSpaceFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_SYSMEM_COLOR_SPACE_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub colorSpace: u32,
}

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Default)]
pub struct BufferCollectionPropertiesFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_BUFFER_COLLECTION_PROPERTIES_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub memoryTypeBits: u32,
    pub bufferCount: u32,
    pub createInfoIndex: u32,
    pub sysmemPixelFormat: u64,
    pub formatFeatures: vk::FormatFeatureFlags,
    pub sysmemColorSpaceIndex: SysmemColorSpaceFUCHSIA,
    #[derivative(Default(value = "unsafe { MaybeUninit::zeroed().assume_init() }"))]
    pub samplerYcbcrConversionComponents: vk::ComponentMapping,
    pub suggestedYcbcrModel: u32,
    pub suggestedYcbcrRange: u32,
    pub suggestedXChromaOffset: u32,
    pub suggestedYChromaOffset: u32,
}

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Default)]
pub struct ImageFormatConstraintsInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_IMAGE_FORMAT_CONSTRAINTS_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    #[derivative(Default(value = "unsafe { MaybeUninit::zeroed().assume_init() }"))]
    pub imageCreateInfo: vk::ImageCreateInfo,
    pub requiredFormatFeatures: vk::FormatFeatureFlags,
    pub flags: ImageFormatConstraintsFlagsFUCHSIA,
    pub sysmemPixelFormat: u64,
    pub colorSpaceCount: u32,
    #[derivative(Default(value = "ptr::null()"))]
    pub pColorSpaces: *const SysmemColorSpaceFUCHSIA,
}

#[repr(C)]
#[allow(non_snake_case)]
#[derive(Derivative)]
#[derivative(Default)]
pub struct ImageConstraintsInfoFUCHSIA {
    #[derivative(Default(value = "STRUCTURE_TYPE_IMAGE_CONSTRAINTS_INFO_FUCHSIA"))]
    pub sType: vk::StructureType,
    #[derivative(Default(value = "ptr::null()"))]
    pub pNext: *const ::std::os::raw::c_void,
    pub formatConstraintsCount: u32,
    #[derivative(Default(value = "ptr::null()"))]
    pub pFormatConstraints: *const ImageFormatConstraintsInfoFUCHSIA,
    pub bufferCollectionConstraints: BufferCollectionConstraintsInfoFUCHSIA,
    pub flags: ImageConstraintsInfoFlagsFUCHSIA,
}

ptrs!(FuchsiaExtensionPointers, {
    // VK_FUCHSIA_external_memory
    GetMemoryZirconHandleFUCHSIA => (
        device: vk::Device,
        pGetZirconHandleInfo: *const MemoryGetZirconHandleInfoFUCHSIA,
        pZirconHandle: *mut zx_handle_t
    ) -> vk::Result,
    GetMemoryZirconHandlePropertiesFUCHSIA => (
        device: vk::Device,
        handleType: u32,
        zirconHandle: zx_handle_t,
        pMemoryZirconHandleProperties: *mut MemoryZirconHandlePropertiesFUCHSIA
    ) -> vk::Result,

    // VK_FUCHSIA_external_semaphore
    ImportSemaphoreZirconHandleFUCHSIA => (
        device: vk::Device,
        pImportSemaphoreZirconHandleInfo: *const ImportSemaphoreZirconHandleInfoFUCHSIA
    ) -> vk::Result,
    GetSemaphoreZirconHandleFUCHSIA => (
        device: vk::Device,
        pGetZirconHandleInfo: *const SemaphoreGetZirconHandleInfoFUCHSIA,
        pZirconHandle: *mut zx_handle_t
    ) -> vk::Result,

    // VK_FUCHSIA_buffer_collection
    CreateBufferCollectionFUCHSIA => (
        device: vk::Device,
        pImportInfo: *const BufferCollectionCreateInfoFUCHSIA,
        pAllocator: *const vk::AllocationCallbacks,
        pCollection: *mut BufferCollectionFUCHSIA) -> vk::Result,
    SetBufferCollectionImageConstraintsFUCHSIA => (
        device: vk::Device,
        collection: BufferCollectionFUCHSIA,
        pImageConstraintsInfo: *const ImageConstraintsInfoFUCHSIA) -> vk::Result,
    SetBufferCollectionBufferConstraintsFUCHSIA => (
        device: vk::Device,
        collection: BufferCollectionFUCHSIA,
        pBufferConstraintsInfo: *const BufferConstraintsInfoFUCHSIA
    ) -> vk::Result,
    DestroyBufferCollectionFUCHSIA => (
        device: vk::Device,
        collection: BufferCollectionFUCHSIA,
        pAllocator: *const vk::AllocationCallbacks
    ) -> (),
    GetBufferCollectionPropertiesFUCHSIA => (
        device: vk::Device,
        collection: BufferCollectionFUCHSIA,
        pProperties: *mut BufferCollectionPropertiesFUCHSIA
    ) -> vk::Result,
});

#[link(name = "vulkan")]
extern "C" {
    fn vkGetInstanceProcAddr(
        instance: vk::Instance,
        pName: *const c_char,
    ) -> vk::PFN_vkVoidFunction;
}

pub fn entry_points() -> vk::EntryPoints {
    vk::EntryPoints::load(|name| unsafe { mem::transmute(vkGetInstanceProcAddr(0, name.as_ptr())) })
}

pub fn instance_pointers(instance: vk::Instance) -> vk::InstancePointers {
    vk::InstancePointers::load(|name| unsafe {
        mem::transmute(vkGetInstanceProcAddr(instance, name.as_ptr()))
    })
}

pub fn device_pointers(vk_i: &vk::InstancePointers, device: vk::Device) -> vk::DevicePointers {
    vk::DevicePointers::load(|name| unsafe {
        vk_i.GetDeviceProcAddr(device, name.as_ptr()) as *const _
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{ffi::CStr, mem::MaybeUninit, os::raw::c_char, ptr};
    use vk_sys as vk;

    macro_rules! vulkan_version {
        ( $major:expr, $minor:expr, $patch:expr ) => {
            ($major as u32) << 22 | ($minor as u32) << 12 | ($patch as u32)
        };
    }

    unsafe fn init<T>(f: impl FnOnce(*mut T)) -> T {
        let mut value = MaybeUninit::uninit();
        f(value.as_mut_ptr());
        value.assume_init()
    }

    fn create_vk_instance() -> vk::Instance {
        let entry_points = super::entry_points();
        let mut instance: vk::Instance = 0;
        let create_instance_result = unsafe {
            entry_points.CreateInstance(
                &vk::InstanceCreateInfo {
                    sType: vk::STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                    pNext: ptr::null(),
                    flags: 0,
                    pApplicationInfo: &vk::ApplicationInfo {
                        sType: vk::STRUCTURE_TYPE_APPLICATION_INFO,
                        pNext: ptr::null(),
                        pApplicationName: c"fuchsia-vulkan unit test".as_ptr(),
                        applicationVersion: 0,
                        pEngineName: ptr::null(),
                        engineVersion: 0,
                        apiVersion: vulkan_version!(1, 1, 0),
                    },
                    enabledLayerCount: 0,
                    ppEnabledLayerNames: ptr::null(),
                    enabledExtensionCount: 0,
                    ppEnabledExtensionNames: ptr::null(),
                },
                ptr::null(),
                &mut instance as *mut vk::Instance,
            )
        };
        assert!(create_instance_result == vk::SUCCESS);
        instance
    }

    fn destroy_vk_instance(instance: vk::Instance) {
        let vk_i = super::instance_pointers(instance);
        unsafe {
            vk_i.DestroyInstance(instance, ptr::null());
        }
    }

    fn get_vk_physical_device(instance: vk::Instance) -> Option<vk::PhysicalDevice> {
        let vk_i = super::instance_pointers(instance);
        let physical_devices = {
            let mut len = unsafe {
                init(|ptr| {
                    vk_i.EnumeratePhysicalDevices(instance, ptr, ptr::null_mut());
                })
            };
            let mut physical_devices: Vec<vk::PhysicalDevice> = Vec::with_capacity(len as usize);
            unsafe {
                let enumerate_result = vk_i.EnumeratePhysicalDevices(
                    instance,
                    &mut len,
                    physical_devices.as_mut_ptr(),
                );
                assert!(enumerate_result == vk::SUCCESS);
                physical_devices.set_len(len as usize);
            }
            physical_devices
        };
        match physical_devices.get(0) {
            Some(val) => Some(*val),
            None => {
                println!("cannot create physical device");
                None
            }
        }
    }

    fn physical_device_supports_extension(
        instance: vk::Instance,
        extension_names: &Vec<*const c_char>,
    ) -> bool {
        let vk_i = super::instance_pointers(instance);
        let physical_device = match get_vk_physical_device(instance) {
            Some(val) => val,
            None => return false,
        };

        let mut extension_len = unsafe {
            init(|ptr| {
                let result = vk_i.EnumerateDeviceExtensionProperties(
                    physical_device,
                    ptr::null(),
                    ptr,
                    ptr::null_mut(),
                );
                assert!(result == vk::SUCCESS);
            })
        };

        let mut extensions: Vec<vk::ExtensionProperties> =
            Vec::with_capacity(extension_len as usize);
        unsafe {
            let result = vk_i.EnumerateDeviceExtensionProperties(
                physical_device,
                ptr::null(),
                &mut extension_len,
                extensions.as_mut_ptr(),
            );
            assert!(result == vk::SUCCESS);
            extensions.set_len(extension_len as usize);
        }

        for expected_extension_name in extension_names {
            match extensions.iter().find(|&p| unsafe {
                CStr::from_ptr(*expected_extension_name)
                    == CStr::from_ptr(&p.extensionName as *const c_char)
            }) {
                None => return false,
                _ => {}
            }
        }
        true
    }

    fn create_vk_device(
        instance: vk::Instance,
        extension_names: Vec<*const c_char>,
    ) -> Option<vk::Device> {
        let vk_i = super::instance_pointers(instance);
        let physical_device = match get_vk_physical_device(instance) {
            Some(val) => val,
            None => return None,
        };

        let mut queue_family_count = unsafe {
            init(|ptr| {
                vk_i.GetPhysicalDeviceQueueFamilyProperties(physical_device, ptr, ptr::null_mut())
            })
        };
        let mut queue_family_properties = Vec::with_capacity(queue_family_count as usize);
        unsafe {
            vk_i.GetPhysicalDeviceQueueFamilyProperties(
                physical_device,
                &mut queue_family_count,
                queue_family_properties.as_mut_ptr(),
            );
            queue_family_properties.set_len(queue_family_count as usize);
        }
        if queue_family_count == 0 {
            println!("no queue family available!");
            return None;
        }

        let queue_priority: f32 = 1.0;
        let queue_create_info = vk::DeviceQueueCreateInfo {
            sType: vk::STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            queueFamilyIndex: 0,
            queueCount: 1,
            pQueuePriorities: &queue_priority as *const f32,
        };

        let device = unsafe {
            init(|ptr| {
                let create_device_result = vk_i.CreateDevice(
                    physical_device,
                    &vk::DeviceCreateInfo {
                        sType: vk::STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                        pNext: ptr::null(),
                        flags: 0,
                        queueCreateInfoCount: 1,
                        pQueueCreateInfos: &queue_create_info as *const _,
                        enabledLayerCount: 0,
                        ppEnabledLayerNames: ptr::null(),
                        enabledExtensionCount: extension_names.len() as u32,
                        ppEnabledExtensionNames: extension_names.as_ptr(),
                        pEnabledFeatures: ptr::null(),
                    },
                    ptr::null(),
                    ptr,
                );
                assert!(create_device_result == vk::SUCCESS);
            })
        };
        Some(device)
    }

    fn destroy_vk_device(instance: vk::Instance, device: vk::Device) {
        let vk_i = super::instance_pointers(instance);
        let vk_d = super::device_pointers(&vk_i, device);
        unsafe {
            vk_d.DestroyDevice(device, ptr::null());
        }
    }

    #[test]
    fn can_create_vk_instance() {
        let instance = create_vk_instance();
        assert!(instance != 0);
    }

    #[test]
    fn can_create_vk_device() {
        let instance = create_vk_instance();
        let device = create_vk_device(instance, vec![]);
        assert!(device.is_some());
        destroy_vk_device(instance, device.unwrap());
        destroy_vk_instance(instance);
    }

    macro_rules! assert_fn_valid {
        ($fn:expr) => {
            assert!($fn as *const i8 != ptr::null(), "function {:?} not found", stringify!($fn));
        };
    }

    #[test]
    fn fuchsia_buffer_collection_function_valid() {
        let instance = create_vk_instance();
        let extension_names = vec![c"VK_FUCHSIA_buffer_collection".as_ptr()];
        if !physical_device_supports_extension(instance, &extension_names) {
            println!("extension {:?} not supported, test skipped.", extension_names);
            return;
        }
        let device = create_vk_device(instance, extension_names).unwrap();

        let vk_i = super::instance_pointers(instance);
        let vk_ext = super::FuchsiaExtensionPointers::load(|name| unsafe {
            vk_i.GetDeviceProcAddr(device, name.as_ptr()) as *const _
        });

        assert_fn_valid!(vk_ext.CreateBufferCollectionFUCHSIA);
        assert_fn_valid!(vk_ext.SetBufferCollectionImageConstraintsFUCHSIA);
        assert_fn_valid!(vk_ext.SetBufferCollectionBufferConstraintsFUCHSIA);
        assert_fn_valid!(vk_ext.DestroyBufferCollectionFUCHSIA);
        assert_fn_valid!(vk_ext.GetBufferCollectionPropertiesFUCHSIA);

        destroy_vk_device(instance, device);
        destroy_vk_instance(instance);
    }

    #[test]
    fn fuchsia_external_memory_function_valid() {
        let instance = create_vk_instance();
        let extension_names = vec![c"VK_FUCHSIA_external_memory".as_ptr()];
        if !physical_device_supports_extension(instance, &extension_names) {
            println!("extension {:?} not supported, test skipped.", extension_names);
            return;
        }
        let device = create_vk_device(instance, extension_names).unwrap();

        let vk_i = super::instance_pointers(instance);
        let vk_ext = super::FuchsiaExtensionPointers::load(|name| unsafe {
            vk_i.GetDeviceProcAddr(device, name.as_ptr()) as *const _
        });

        assert_fn_valid!(vk_ext.GetMemoryZirconHandleFUCHSIA);
        assert_fn_valid!(vk_ext.GetMemoryZirconHandlePropertiesFUCHSIA);

        destroy_vk_device(instance, device);
        destroy_vk_instance(instance);
    }

    #[test]
    fn fuchsia_external_semaphore_function_valid() {
        let instance = create_vk_instance();
        let extension_names = vec![c"VK_FUCHSIA_external_semaphore".as_ptr()];
        if !physical_device_supports_extension(instance, &extension_names) {
            println!("extension {:?} not supported, test skipped.", extension_names);
            return;
        }
        let device = create_vk_device(instance, extension_names).unwrap();

        let vk_i = super::instance_pointers(instance);
        let vk_ext = super::FuchsiaExtensionPointers::load(|name| unsafe {
            vk_i.GetDeviceProcAddr(device, name.as_ptr()) as *const _
        });

        assert_fn_valid!(vk_ext.ImportSemaphoreZirconHandleFUCHSIA);
        assert_fn_valid!(vk_ext.GetSemaphoreZirconHandleFUCHSIA);

        destroy_vk_device(instance, device);
        destroy_vk_instance(instance);
    }
}
