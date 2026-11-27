use openh264::formats::YUVSource;

/// High-speed planar I420 (YUV 4:2:0) buffer with precomputed scaling Lookup Tables (LUTs).
/// Performs Crop + Scale + BGRA-to-I420 conversion in a single pass with zero per-frame allocations.
pub struct FastYuvBuffer {
    width: usize,
    height: usize,
    y_plane: Vec<u8>,
    u_plane: Vec<u8>,
    v_plane: Vec<u8>,
    x_lut: Vec<usize>,
    y_lut: Vec<usize>,
    cached_lut_key: (u32, u32, i32, i32, i32, i32, usize, usize),
    pub has_content: bool,
}

impl FastYuvBuffer {
    pub fn new(width: usize, height: usize) -> Self {
        let w = (width / 2) * 2;
        let h = (height / 2) * 2;
        let y_size = w * h;
        let uv_size = (w / 2) * (h / 2);

        Self {
            width: w,
            height: h,
            y_plane: vec![16u8; y_size],
            u_plane: vec![128u8; uv_size],
            v_plane: vec![128u8; uv_size],
            x_lut: vec![0; w],
            y_lut: vec![0; h],
            cached_lut_key: (0, 0, 0, 0, 0, 0, 0, 0),
            has_content: false,
        }
    }

    fn ensure_luts(&mut self, src_w: u32, src_h: u32, crop: (i32, i32, i32, i32)) -> bool {
        let (left, top, width, height) = crop;
        if width <= 0 || height <= 0 || src_w == 0 || src_h == 0 {
            return false;
        }

        let right = left.saturating_add(width);
        let bottom = top.saturating_add(height);
        if right <= 0 || bottom <= 0 || left >= src_w as i32 || top >= src_h as i32 {
            return false;
        }

        let key = (
            src_w, src_h, left, top, width, height, self.width, self.height,
        );
        if self.cached_lut_key == key {
            return true;
        }

        let src_w_usize = src_w as usize;
        let src_h_usize = src_h as usize;

        let crop_x0 = (left.max(0) as usize).min(src_w_usize.saturating_sub(1));
        let crop_y0 = (top.max(0) as usize).min(src_h_usize.saturating_sub(1));
        let crop_x1 = (right.max(1) as usize).min(src_w_usize).max(crop_x0 + 1);
        let crop_y1 = (bottom.max(1) as usize).min(src_h_usize).max(crop_y0 + 1);
        let crop_w = (crop_x1 - crop_x0).max(1);
        let crop_h = (crop_y1 - crop_y0).max(1);

        for x in 0..self.width {
            let sx = crop_x0 + (x * crop_w) / self.width;
            self.x_lut[x] = sx.min(src_w_usize - 1) * 4;
        }

        let src_stride = src_w_usize * 4;
        for y in 0..self.height {
            let sy = crop_y0 + (y * crop_h) / self.height;
            self.y_lut[y] = sy.min(src_h_usize - 1) * src_stride;
        }

        self.cached_lut_key = key;
        true
    }

    /// Converts a cropped region of a BGRA buffer directly into planar I420 (Y, U, V) in one pass.
    pub fn convert_cropped_bgra_to_i420(
        &mut self,
        src: &[u8],
        src_w: u32,
        src_h: u32,
        crop: (i32, i32, i32, i32),
    ) {
        let required_len = (src_w as usize) * (src_h as usize) * 4;
        if src.len() < required_len || !self.ensure_luts(src_w, src_h, crop) {
            return;
        }

        let dst_w = self.width;
        let dst_h = self.height;
        let uv_stride = dst_w / 2;

        // Safety invariant: ensure_luts guarantees y_lut.len() == dst_h with max value (src_h - 1) * src_w * 4,
        // and x_lut.len() == dst_w with max value (src_w - 1) * 4, and src.len() >= src_w * src_h * 4.
        unsafe {
            let src_ptr = src.as_ptr();
            let y_lut_ptr = self.y_lut.as_ptr();
            let x_lut_ptr = self.x_lut.as_ptr();
            let y_ptr = self.y_plane.as_mut_ptr();
            let u_ptr = self.u_plane.as_mut_ptr();
            let v_ptr = self.v_plane.as_mut_ptr();

            let mut y_pair = 0;
            while y_pair + 1 < dst_h {
                let src_row0 = *y_lut_ptr.add(y_pair);
                let src_row1 = *y_lut_ptr.add(y_pair + 1);
                let dst_y0 = y_pair * dst_w;
                let dst_y1 = (y_pair + 1) * dst_w;
                let dst_uv_row = (y_pair / 2) * uv_stride;

                let mut x_pair = 0;
                while x_pair + 1 < dst_w {
                    let sx0 = *x_lut_ptr.add(x_pair);
                    let sx1 = *x_lut_ptr.add(x_pair + 1);

                    // Top-left (0, 0)
                    let p00 = src_row0 + sx0;
                    let b00 = *src_ptr.add(p00) as i32;
                    let g00 = *src_ptr.add(p00 + 1) as i32;
                    let r00 = *src_ptr.add(p00 + 2) as i32;
                    *y_ptr.add(dst_y0 + x_pair) =
                        (((66 * r00 + 129 * g00 + 25 * b00 + 128) >> 8) + 16) as u8;

                    // Top-right (1, 0)
                    let p10 = src_row0 + sx1;
                    let b10 = *src_ptr.add(p10) as i32;
                    let g10 = *src_ptr.add(p10 + 1) as i32;
                    let r10 = *src_ptr.add(p10 + 2) as i32;
                    *y_ptr.add(dst_y0 + x_pair + 1) =
                        (((66 * r10 + 129 * g10 + 25 * b10 + 128) >> 8) + 16) as u8;

                    // Bottom-left (0, 1)
                    let p01 = src_row1 + sx0;
                    let b01 = *src_ptr.add(p01) as i32;
                    let g01 = *src_ptr.add(p01 + 1) as i32;
                    let r01 = *src_ptr.add(p01 + 2) as i32;
                    *y_ptr.add(dst_y1 + x_pair) =
                        (((66 * r01 + 129 * g01 + 25 * b01 + 128) >> 8) + 16) as u8;

                    // Bottom-right (1, 1)
                    let p11 = src_row1 + sx1;
                    let b11 = *src_ptr.add(p11) as i32;
                    let g11 = *src_ptr.add(p11 + 1) as i32;
                    let r11 = *src_ptr.add(p11 + 2) as i32;
                    *y_ptr.add(dst_y1 + x_pair + 1) =
                        (((66 * r11 + 129 * g11 + 25 * b11 + 128) >> 8) + 16) as u8;

                    // Subsampled 2x2 Chroma (U, V)
                    let r_avg = (r00 + r11) >> 1;
                    let g_avg = (g00 + g11) >> 1;
                    let b_avg = (b00 + b11) >> 1;
                    let uv_idx = dst_uv_row + (x_pair / 2);
                    *u_ptr.add(uv_idx) =
                        (((-38 * r_avg - 74 * g_avg + 112 * b_avg + 128) >> 8) + 128) as u8;
                    *v_ptr.add(uv_idx) =
                        (((112 * r_avg - 94 * g_avg - 18 * b_avg + 128) >> 8) + 128) as u8;

                    x_pair += 2;
                }
                y_pair += 2;
            }
        }
        self.has_content = true;
    }

    /// Renders a clean dark studio standby pattern when waiting for a minimized/closed target window
    /// (ensuring Window Mode NEVER leaks the user's desktop).
    pub fn render_standby_pattern(&mut self, frame_counter: u64) {
        let dst_w = self.width;
        let dst_h = self.height;
        let uv_w = dst_w / 2;
        let uv_h = dst_h / 2;
        let bar_pos = ((frame_counter * 6) as usize) % dst_w;

        for y in 0..dst_h {
            let row = y * dst_w;
            let in_band = y > dst_h * 2 / 5 && y < dst_h * 3 / 5;
            let base_y = 24 + ((y * 18) / dst_h) as u8;
            for x in 0..dst_w {
                let pulse = in_band && x >= bar_pos && x < (bar_pos + 96).min(dst_w);
                self.y_plane[row + x] = if pulse { 110 } else { base_y };
            }
        }
        self.u_plane[..uv_w * uv_h].fill(134);
        self.v_plane[..uv_w * uv_h].fill(124);
        self.has_content = true;
    }
}

impl YUVSource for FastYuvBuffer {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }

    fn y(&self) -> &[u8] {
        &self.y_plane
    }

    fn u(&self) -> &[u8] {
        &self.u_plane
    }

    fn v(&self) -> &[u8] {
        &self.v_plane
    }
}

#[cfg(windows)]
pub mod dxgi {
    use windows::core::Interface;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
        D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
        DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_INVALID_CALL, DXGI_ERROR_WAIT_TIMEOUT,
        DXGI_OUTDUPL_FRAME_INFO,
    };

    pub struct DxgiCapture {
        pub _device: ID3D11Device,
        pub context: ID3D11DeviceContext,
        pub duplication: IDXGIOutputDuplication,
        pub staging: ID3D11Texture2D,
        pub width: u32,
        pub height: u32,
        pub pixel_buffer: Vec<u8>,
        pub has_valid_frame: bool,
    }

    impl DxgiCapture {
        pub fn new(display_index: u32) -> windows::core::Result<Self> {
            unsafe {
                let factory: IDXGIFactory1 = CreateDXGIFactory1()?;

                // Find the adapter that owns the requested display output
                let mut adapter_idx = 0;
                let mut found = None;
                while let Ok(adapter) = factory.EnumAdapters1(adapter_idx) {
                    adapter_idx += 1;
                    if let Ok(output) = adapter.EnumOutputs(display_index) {
                        found = Some((adapter, output));
                        break;
                    }
                }

                let (adapter, output) = match found {
                    Some(pair) => pair,
                    None => {
                        let adapter = factory.EnumAdapters1(0)?;
                        let output = adapter.EnumOutputs(0)?;
                        (adapter, output)
                    }
                };

                let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
                let mut device_opt: Option<ID3D11Device> = None;
                let mut context_opt: Option<ID3D11DeviceContext> = None;
                let mut obtained_level = D3D_FEATURE_LEVEL(0);

                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&feature_levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device_opt),
                    Some(&mut obtained_level),
                    Some(&mut context_opt),
                )?;

                let device = device_opt.expect("D3D11 Device should be present on success");
                let context = context_opt.expect("D3D11 Context should be present on success");

                let output1: IDXGIOutput1 = output.cast()?;
                let output_desc = output.GetDesc()?;

                let width = (output_desc.DesktopCoordinates.right
                    - output_desc.DesktopCoordinates.left) as u32;
                let height = (output_desc.DesktopCoordinates.bottom
                    - output_desc.DesktopCoordinates.top) as u32;

                let duplication = output1.DuplicateOutput(&device)?;

                let staging_desc = D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };

                let mut staging_opt = None;
                device.CreateTexture2D(&staging_desc, None, Some(&mut staging_opt))?;
                let staging = staging_opt.expect("Staging texture should be created");

                let buffer_size = (width * height * 4) as usize;
                let pixel_buffer = vec![0u8; buffer_size];

                Ok(Self {
                    _device: device,
                    context,
                    duplication,
                    staging,
                    width,
                    height,
                    pixel_buffer,
                    has_valid_frame: false,
                })
            }
        }

        /// Acquires the latest desktop frame if updated.
        /// Returns `Ok(true)` if new pixels were copied, `Ok(false)` if unchanged since last frame.
        pub fn acquire_pixels(&mut self, timeout_ms: u32) -> windows::core::Result<bool> {
            unsafe {
                let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource = None;

                let res = self.duplication.AcquireNextFrame(
                    timeout_ms,
                    &mut frame_info,
                    &mut resource,
                );

                match res {
                    Ok(_) => {
                        // If only mouse pointer metadata changed without a desktop image update,
                        // release immediately and avoid an expensive 8MB PCIe readback!
                        if frame_info.LastPresentTime == 0 && self.has_valid_frame {
                            let _ = self.duplication.ReleaseFrame();
                            return Ok(false);
                        }

                        let res_obj = match resource {
                            Some(r) => r,
                            None => {
                                let _ = self.duplication.ReleaseFrame();
                                return Ok(false);
                            }
                        };

                        let texture: ID3D11Texture2D = match res_obj.cast() {
                            Ok(t) => t,
                            Err(e) => {
                                let _ = self.duplication.ReleaseFrame();
                                return Err(e);
                            }
                        };

                        self.context.CopyResource(&self.staging, &texture);
                        let _ = self.duplication.ReleaseFrame();

                        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                        self.context.Map(
                            &self.staging,
                            0,
                            D3D11_MAP_READ,
                            0,
                            Some(&mut mapped),
                        )?;

                        let row_bytes = (self.width * 4) as usize;
                        let src_ptr = mapped.pData as *const u8;
                        let pitch = mapped.RowPitch as usize;

                        if pitch == row_bytes {
                            std::ptr::copy_nonoverlapping(
                                src_ptr,
                                self.pixel_buffer.as_mut_ptr(),
                                row_bytes * (self.height as usize),
                            );
                        } else {
                            for y in 0..self.height as usize {
                                let src_row = src_ptr.add(y * pitch);
                                let dst_start = y * row_bytes;
                                std::ptr::copy_nonoverlapping(
                                    src_row,
                                    self.pixel_buffer.as_mut_ptr().add(dst_start),
                                    row_bytes,
                                );
                            }
                        }

                        self.context.Unmap(&self.staging, 0);
                        self.has_valid_frame = true;
                        Ok(true)
                    }
                    Err(e) => {
                        if e.code() == DXGI_ERROR_WAIT_TIMEOUT {
                            Ok(false)
                        } else if e.code() == DXGI_ERROR_ACCESS_LOST
                            || e.code() == DXGI_ERROR_INVALID_CALL
                        {
                            Err(e)
                        } else {
                            Err(e)
                        }
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
pub mod win_capture {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetWindowDC,
        ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
        HDC, HGDIOBJ, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect};

    #[link(name = "user32")]
    unsafe extern "system" {
        fn PrintWindow(hwnd: isize, hdc_blt: isize, n_flags: u32) -> i32;
    }

    const PW_CLIENTONLY: u32 = 0x0000_0001;
    const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;

    /// Dedicated per-window capturer using DWM `PrintWindow(PW_RENDERFULLCONTENT)` and GDI `BitBlt`.
    /// Captures ONLY the target application window even when occluded by other windows,
    /// and never captures the surrounding desktop.
    pub struct WindowCapturer {
        mem_dc: HDC,
        dib_bitmap: HBITMAP,
        old_obj: HGDIOBJ,
        dib_ptr: *mut u8,
        pub width: u32,
        pub height: u32,
        pub pixel_buffer: Vec<u8>,
    }

    unsafe impl Send for WindowCapturer {}

    impl WindowCapturer {
        pub fn new() -> Self {
            Self {
                mem_dc: HDC::default(),
                dib_bitmap: HBITMAP::default(),
                old_obj: HGDIOBJ::default(),
                dib_ptr: std::ptr::null_mut(),
                width: 0,
                height: 0,
                pixel_buffer: Vec::new(),
            }
        }

        fn ensure_surface(&mut self, w: u32, h: u32) -> bool {
            if w == 0 || h == 0 {
                return false;
            }
            if self.width == w && self.height == h && !self.dib_ptr.is_null() {
                return true;
            }

            self.cleanup();

            unsafe {
                let screen_dc = GetDC(None);
                let mem_dc = CreateCompatibleDC(Some(screen_dc));
                ReleaseDC(None, screen_dc);

                if mem_dc.is_invalid() {
                    return false;
                }

                let bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w as i32,
                        biHeight: -(h as i32), // Top-down BGRA bitmap
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        biSizeImage: w * h * 4,
                        ..Default::default()
                    },
                    ..Default::default()
                };

                let mut bits_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
                let dib_res =
                    CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0);

                match dib_res {
                    Ok(dib) if !dib.is_invalid() && !bits_ptr.is_null() => {
                        let old = SelectObject(mem_dc, dib.into());
                        self.mem_dc = mem_dc;
                        self.dib_bitmap = dib;
                        self.old_obj = old;
                        self.dib_ptr = bits_ptr as *mut u8;
                        self.width = w;
                        self.height = h;
                        self.pixel_buffer.resize((w * h * 4) as usize, 0);
                        true
                    }
                    _ => {
                        let _ = DeleteDC(mem_dc);
                        false
                    }
                }
            }
        }

        /// Captures the target window (`hwnd_val`) into `self.pixel_buffer`.
        /// Returns `true` if a valid non-empty window image was captured.
        pub fn capture_window(&mut self, hwnd_val: isize) -> bool {
            if !crate::win_enum::is_window_handle_valid(hwnd_val)
                || crate::win_enum::is_window_minimized(hwnd_val)
            {
                return false;
            }

            unsafe {
                let hwnd = HWND(hwnd_val as *mut _);

                // Prefer client area dimensions so game content fills the frame cleanly
                let mut client_rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut client_rect);
                let client_w = (client_rect.right - client_rect.left).max(0) as u32;
                let client_h = (client_rect.bottom - client_rect.top).max(0) as u32;

                let mut win_rect = RECT::default();
                let _ = GetWindowRect(hwnd, &mut win_rect);
                let win_w = (win_rect.right - win_rect.left).max(0) as u32;
                let win_h = (win_rect.bottom - win_rect.top).max(0) as u32;

                let (use_client, cap_w, cap_h) = if client_w > 64 && client_h > 64 {
                    (true, client_w, client_h)
                } else if win_w > 64 && win_h > 64 {
                    (false, win_w, win_h)
                } else {
                    return false;
                };

                if !self.ensure_surface(cap_w, cap_h) {
                    return false;
                }

                let total_bytes = (cap_w * cap_h * 4) as usize;

                // Stage 1: DWM PrintWindow with PW_RENDERFULLCONTENT (captures Direct3D/OpenGL/GDI windows)
                Self::clear_sample_canaries(self.dib_ptr, total_bytes);
                let flags = if use_client {
                    PW_CLIENTONLY | PW_RENDERFULLCONTENT
                } else {
                    PW_RENDERFULLCONTENT
                };
                let ok_pw = PrintWindow(hwnd_val, self.mem_dc.0 as isize, flags) != 0;
                if ok_pw && Self::buffer_has_visible_pixels(self.dib_ptr, total_bytes) {
                    std::ptr::copy_nonoverlapping(
                        self.dib_ptr,
                        self.pixel_buffer.as_mut_ptr(),
                        total_bytes,
                    );
                    return true;
                }

                // Stage 2: Client DC / Window DC BitBlt fallback (for classic GDI/DirectDraw games like Plants vs Zombies)
                Self::clear_sample_canaries(self.dib_ptr, total_bytes);
                let src_dc = if use_client {
                    GetDC(Some(hwnd))
                } else {
                    GetWindowDC(Some(hwnd))
                };
                if !src_dc.is_invalid() {
                    let blt_ok = BitBlt(
                        self.mem_dc,
                        0,
                        0,
                        cap_w as i32,
                        cap_h as i32,
                        Some(src_dc),
                        0,
                        0,
                        SRCCOPY,
                    )
                    .is_ok();
                    ReleaseDC(Some(hwnd), src_dc);

                    if blt_ok && Self::buffer_has_visible_pixels(self.dib_ptr, total_bytes) {
                        std::ptr::copy_nonoverlapping(
                            self.dib_ptr,
                            self.pixel_buffer.as_mut_ptr(),
                            total_bytes,
                        );
                        return true;
                    }
                }

                // Stage 3: Standard PrintWindow (flags = 0)
                Self::clear_sample_canaries(self.dib_ptr, total_bytes);
                if PrintWindow(hwnd_val, self.mem_dc.0 as isize, 0) != 0
                    && Self::buffer_has_visible_pixels(self.dib_ptr, total_bytes)
                {
                    std::ptr::copy_nonoverlapping(
                        self.dib_ptr,
                        self.pixel_buffer.as_mut_ptr(),
                        total_bytes,
                    );
                    return true;
                }

                false
            }
        }

        /// Captures the primary desktop screen via GDI `BitBlt` (used to seed Frame 1 when the desktop is 100% static
        /// and DXGI Desktop Duplication is waiting for the next screen change).
        pub fn capture_primary_screen(&mut self, screen_w: u32, screen_h: u32) -> bool {
            if !self.ensure_surface(screen_w, screen_h) {
                return false;
            }
            let total_bytes = (screen_w * screen_h * 4) as usize;
            unsafe {
                Self::clear_sample_canaries(self.dib_ptr, total_bytes);
                let screen_dc = GetDC(None);
                if screen_dc.is_invalid() {
                    return false;
                }
                let blt_ok = BitBlt(
                    self.mem_dc,
                    0,
                    0,
                    screen_w as i32,
                    screen_h as i32,
                    Some(screen_dc),
                    0,
                    0,
                    SRCCOPY,
                )
                .is_ok();
                ReleaseDC(None, screen_dc);
                if blt_ok && Self::buffer_has_visible_pixels(self.dib_ptr, total_bytes) {
                    std::ptr::copy_nonoverlapping(
                        self.dib_ptr,
                        self.pixel_buffer.as_mut_ptr(),
                        total_bytes,
                    );
                    return true;
                }
                false
            }
        }

        /// Zeroes the sampled interior canary positions before calling `PrintWindow` or `BitBlt`
        /// so stale pixels from an earlier frame can never cause a false-positive capture check.
        fn clear_sample_canaries(ptr: *mut u8, len: usize) {
            if ptr.is_null() || len < 256 {
                return;
            }
            let margin = (len / 16) & !3;
            let usable = len.saturating_sub(margin * 2);
            let step = (usable / 128).max(4) & !3;
            unsafe {
                let mut idx = margin;
                while idx + 3 < len - margin {
                    *ptr.add(idx) = 0;
                    *ptr.add(idx + 1) = 0;
                    *ptr.add(idx + 2) = 0;
                    *ptr.add(idx + 3) = 0;
                    idx += step;
                }
            }
        }

        fn buffer_has_visible_pixels(ptr: *const u8, len: usize) -> bool {
            if ptr.is_null() || len < 256 {
                return false;
            }
            let margin = (len / 16) & !3;
            let usable = len.saturating_sub(margin * 2);
            let step = (usable / 128).max(4) & !3;
            let mut non_zero_count = 0u32;
            unsafe {
                let mut idx = margin;
                while idx + 3 < len - margin {
                    let b = *ptr.add(idx);
                    let g = *ptr.add(idx + 1);
                    let r = *ptr.add(idx + 2);
                    if (b | g | r) != 0 {
                        non_zero_count += 1;
                        if non_zero_count >= 4 {
                            return true;
                        }
                    }
                    idx += step;
                }
            }
            false
        }

        fn cleanup(&mut self) {
            unsafe {
                if !self.mem_dc.is_invalid() {
                    if !self.old_obj.is_invalid() {
                        let _ = SelectObject(self.mem_dc, self.old_obj);
                        self.old_obj = HGDIOBJ::default();
                    }
                    if !self.dib_bitmap.is_invalid() {
                        let _ = DeleteObject(self.dib_bitmap.into());
                        self.dib_bitmap = HBITMAP::default();
                    }
                    let _ = DeleteDC(self.mem_dc);
                    self.mem_dc = HDC::default();
                }
                self.dib_ptr = std::ptr::null_mut();
                self.width = 0;
                self.height = 0;
            }
        }
    }

    impl Drop for WindowCapturer {
        fn drop(&mut self) {
            self.cleanup();
        }
    }
}

pub struct VideoFrameSource {
    pub capture_mode: String,
    pub target_window_title: String,
    pub target_window_hwnd: isize,
    #[cfg(windows)]
    pub dxgi: Option<dxgi::DxgiCapture>,
    #[cfg(windows)]
    pub win_capturer: win_capture::WindowCapturer,
    pub frame_counter: u64,
}

impl VideoFrameSource {
    pub fn new(
        _target_width: u32,
        _target_height: u32,
        capture_mode: String,
        target_window_title: String,
        target_window_hwnd: isize,
    ) -> Self {
        crate::win_enum::attach_to_input_desktop();

        #[cfg(windows)]
        let dxgi = if capture_mode == "Desktop" {
            dxgi::DxgiCapture::new(0).ok()
        } else {
            None
        };

        let resolved_hwnd = if capture_mode == "Window" {
            crate::win_enum::resolve_target_window(target_window_hwnd, &target_window_title)
                .map(|w| w.hwnd)
                .unwrap_or(target_window_hwnd)
        } else {
            0
        };

        Self {
            capture_mode,
            target_window_title,
            target_window_hwnd: resolved_hwnd,
            #[cfg(windows)]
            dxgi,
            #[cfg(windows)]
            win_capturer: win_capture::WindowCapturer::new(),
            frame_counter: 0,
        }
    }

    /// Captures the next video frame and populates `yuv_out` directly in a single pass.
    pub fn capture_into_yuv(&mut self, yuv_out: &mut FastYuvBuffer) {
        if self.frame_counter == 0 {
            crate::win_enum::attach_to_input_desktop();
        }
        self.frame_counter += 1;

        #[cfg(windows)]
        {
            if self.capture_mode == "Window" {
                self.capture_window_into_yuv(yuv_out);
                return;
            } else {
                if self.capture_desktop_into_yuv(yuv_out) {
                    return;
                }
            }
        }

        // Fallback animated studio canvas only if no display adapter is available
        yuv_out.render_standby_pattern(self.frame_counter);
    }

    #[cfg(windows)]
    fn capture_window_into_yuv(&mut self, yuv_out: &mut FastYuvBuffer) {
        // Re-resolve HWND at most once per second if invalid or closed
        if !crate::win_enum::is_window_handle_valid(self.target_window_hwnd)
            && (self.frame_counter == 1 || self.frame_counter % 30 == 0)
        {
            if let Some(win) = crate::win_enum::resolve_target_window(
                self.target_window_hwnd,
                &self.target_window_title,
            ) {
                self.target_window_hwnd = win.hwnd;
            }
        }

        let hwnd = self.target_window_hwnd;
        if hwnd != 0 && !crate::win_enum::is_window_minimized(hwnd) {
            // 1. Primary path: Direct Window Capture via PrintWindow(PW_RENDERFULLCONTENT) / Window DC BitBlt
            if self.win_capturer.capture_window(hwnd) {
                let w = self.win_capturer.width;
                let h = self.win_capturer.height;
                yuv_out.convert_cropped_bgra_to_i420(
                    &self.win_capturer.pixel_buffer,
                    w,
                    h,
                    (0, 0, w as i32, h as i32),
                );
                return;
            }

            // 2. Secondary path: If direct window surface is exclusive/protected, crop ONLY the target window's client rect from DXGI
            if let Some(rect) = crate::win_enum::get_window_dimensions(hwnd) {
                if self.dxgi.is_none() {
                    self.dxgi = dxgi::DxgiCapture::new(0).ok();
                }
                if let Some(dxgi) = &mut self.dxgi {
                    let timeout = if dxgi.has_valid_frame { 0 } else { 16 };
                    if dxgi.acquire_pixels(timeout).is_err() {
                        self.dxgi = dxgi::DxgiCapture::new(0).ok();
                    } else if dxgi.has_valid_frame {
                        yuv_out.convert_cropped_bgra_to_i420(
                            &dxgi.pixel_buffer,
                            dxgi.width,
                            dxgi.height,
                            rect,
                        );
                        return;
                    }
                }
            }
        }

        // If the window is minimized (e.g. user Alt-Tabbed to ObsLite) and we already have a captured frame,
        // keep the last valid window frame frozen rather than ever showing the desktop!
        if !yuv_out.has_content {
            yuv_out.render_standby_pattern(self.frame_counter);
        }
    }

    #[cfg(windows)]
    fn capture_desktop_into_yuv(&mut self, yuv_out: &mut FastYuvBuffer) -> bool {
        if self.dxgi.is_none() && self.frame_counter % 60 == 0 {
            self.dxgi = dxgi::DxgiCapture::new(0).ok();
        }

        if let Some(dxgi) = &mut self.dxgi {
            let timeout = if dxgi.has_valid_frame { 0 } else { 8 };
            match dxgi.acquire_pixels(timeout) {
                Ok(true) => {
                    let w = dxgi.width;
                    let h = dxgi.height;
                    yuv_out.convert_cropped_bgra_to_i420(
                        &dxgi.pixel_buffer,
                        w,
                        h,
                        (0, 0, w as i32, h as i32),
                    );
                    return true;
                }
                Ok(false) => {
                    // Screen did not change since last frame!
                    if yuv_out.has_content {
                        return true;
                    } else if dxgi.has_valid_frame {
                        let w = dxgi.width;
                        let h = dxgi.height;
                        yuv_out.convert_cropped_bgra_to_i420(
                            &dxgi.pixel_buffer,
                            w,
                            h,
                            (0, 0, w as i32, h as i32),
                        );
                        return true;
                    } else {
                        // Static desktop on Frame 1 before first DXGI update: seed initial frame via GDI screen BitBlt
                        let w = dxgi.width;
                        let h = dxgi.height;
                        if self.win_capturer.capture_primary_screen(w, h) {
                            yuv_out.convert_cropped_bgra_to_i420(
                                &self.win_capturer.pixel_buffer,
                                w,
                                h,
                                (0, 0, w as i32, h as i32),
                            );
                            return true;
                        }
                    }
                }
                Err(_) => {
                    // DXGI_ERROR_ACCESS_LOST (e.g., fullscreen transition) — recreate DXGI session on next tick
                    tracing::info!("DXGI desktop duplication access lost; scheduling re-init...");
                    self.dxgi = dxgi::DxgiCapture::new(0).ok();
                    if yuv_out.has_content {
                        return true;
                    }
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn test_fast_yuv_buffer_single_pass_conversion_and_speed() {
        let src_w = 1920u32;
        let src_h = 1080u32;
        let mut src_bgra = vec![0u8; (src_w * src_h * 4) as usize];
        for (i, byte) in src_bgra.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }

        let mut yuv = FastYuvBuffer::new(1280, 720);
        let start = Instant::now();
        for _ in 0..10 {
            yuv.convert_cropped_bgra_to_i420(
                &src_bgra,
                src_w,
                src_h,
                (0, 0, src_w as i32, src_h as i32),
            );
        }
        let avg_ms = start.elapsed().as_secs_f64() * 100.0;
        println!("Single-pass 1080p->720p BGRA->I420 avg time: {:.2} ms", avg_ms);

        assert!(yuv.has_content);
        assert_eq!(yuv.dimensions(), (1280, 720));
        assert_eq!(yuv.y().len(), 1280 * 720);
        assert_eq!(yuv.u().len(), 640 * 360);
        assert_eq!(yuv.v().len(), 640 * 360);
        // Should easily run in < 15ms per frame
        assert!(avg_ms < 25.0);
    }

    #[test]
    fn test_window_crop_conversion_does_not_panic_on_edge_rects() {
        let src_w = 1280u32;
        let src_h = 720u32;
        let src_bgra = vec![128u8; (src_w * src_h * 4) as usize];
        let mut yuv = FastYuvBuffer::new(1280, 720);

        // Negative offsets and out-of-bounds dimensions
        yuv.convert_cropped_bgra_to_i420(&src_bgra, src_w, src_h, (-50, -50, 2000, 2000));
        assert!(yuv.has_content);

        // Zero size rect should safely no-op
        yuv.convert_cropped_bgra_to_i420(&src_bgra, src_w, src_h, (100, 100, 0, 0));
    }

    #[test]
    fn test_negative_window_crop_does_not_leak_desktop_outside_window() {
        let src_w = 1000u32;
        let src_h = 600u32;
        let mut src_bgra = vec![0u8; (src_w * src_h * 4) as usize];
        // Fill x in 0..600 with bright white (B=255, G=255, R=255) and x in 600..1000 with pure black (0, 0, 0)
        for y in 0..src_h as usize {
            for x in 0..src_w as usize {
                let idx = (y * (src_w as usize) + x) * 4;
                if x < 600 {
                    src_bgra[idx] = 255;
                    src_bgra[idx + 1] = 255;
                    src_bgra[idx + 2] = 255;
                    src_bgra[idx + 3] = 255;
                }
            }
        }

        // Window is at left = -200, width = 800 -> right edge on screen is at x = 600.
        // Cropping must only sample x in 0..600 (all white, Y = 235) and NEVER sample x >= 600 (black, Y = 16).
        let mut yuv = FastYuvBuffer::new(800, 600);
        yuv.convert_cropped_bgra_to_i420(&src_bgra, src_w, src_h, (-200, 0, 800, 600));
        assert!(yuv.has_content);
        assert!(
            yuv.y().iter().all(|&y_val| y_val >= 230),
            "Off-screen window crop leaked black desktop pixels from x >= 600!"
        );
    }
}
