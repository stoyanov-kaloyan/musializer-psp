use crate::utils::AssetStream;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use core::{ffi::c_void, ptr};
use psp::sys::{
    self, AudioOutputFrequency, Mp3Handle, sceMp3CheckStreamDataNeeded, sceMp3Decode,
    sceMp3GetInfoToAddStreamData, sceMp3Init, sceMp3InitResource, sceMp3NotifyAddStreamData,
    sceMp3ReleaseMp3Handle, sceMp3ReserveMp3Handle, sceMp3SetLoopNum, sceMp3TermResource,
};

// TODO: make it loop

extern crate alloc;
use alloc::{boxed::Box, string::String};

#[repr(C, align(64))]
struct Align64<T>(T);

const MP3_BUF_SIZE: usize = 16 * 1024; // 16KB for MP3 stream data
const PCM_BUF_SIZE: usize = 16 * (1152 / 2); // PCM output buffer

static mut MP3_BUF: Align64<[u8; MP3_BUF_SIZE]> = Align64([0; MP3_BUF_SIZE]);
static mut PCM_BUF: Align64<[u8; PCM_BUF_SIZE]> = Align64([0; PCM_BUF_SIZE]);

/// Find the start of the actual MP3 stream by skipping metadata tags (ID3v2, APE)
/// Returns the byte offset where the MP3 audio data begins
fn find_stream_start(stream: &mut AssetStream) -> Result<u32, i32> {
    let mut header = [0u8; 32];

    stream.seek(0, sys::IoWhence::Set)?;

    let n = stream.read(&mut header)?;
    if n < 10 {
        return Ok(0);
    }

    if header[0] == b'I' && header[1] == b'D' && header[2] == b'3' {
        let size = ((header[6] as u32 & 0x7F) << 21)
            | ((header[7] as u32 & 0x7F) << 14)
            | ((header[8] as u32 & 0x7F) << 7)
            | (header[9] as u32 & 0x7F);
        return Ok(size + 10);
    }

    if header[0] == b'A' && header[1] == b'P' && header[2] == b'E' && header[3] == b'T' {
        let size = (header[12] as u32)
            | ((header[13] as u32) << 8)
            | ((header[14] as u32) << 16)
            | ((header[15] as u32) << 24);
        return Ok(size + 32);
    }

    Ok(0)
}

/// Fill the MP3 stream buffer from the file
/// Returns true if there's more data, false if we hit the start of file
fn fill_stream_buffer(fd: &mut AssetStream, handle: Mp3Handle) -> Result<bool, i32> {
    let mut dst: *mut u8 = ptr::null_mut();
    let mut to_write: i32 = 0;
    let mut src_pos: i32 = 0;

    let status =
        unsafe { sceMp3GetInfoToAddStreamData(handle, &mut dst, &mut to_write, &mut src_pos) };
    if status < 0 {
        return Err(status);
    }

    let seek_result = fd.seek(src_pos as i64, sys::IoWhence::Set);
    if seek_result.is_err() {
        return Err(seek_result.unwrap_err());
    }

    let buf = unsafe { core::slice::from_raw_parts_mut(dst, to_write as usize) };
    let read = fd.read(buf)?;

    if read == 0 {
        // EOF reached
        let _ = unsafe { sceMp3NotifyAddStreamData(handle, 0) };
        return Ok(false);
    }

    let status = unsafe { sceMp3NotifyAddStreamData(handle, read as i32) };
    if status < 0 {
        return Err(status);
    }

    Ok(src_pos > 0)
}

/// Feed the MP3 decoder if it needs more data
fn mp3_feed(instance: &mut Mp3Instance) -> Result<bool, i32> {
    if instance.error || instance.paused {
        return Ok(false);
    }

    let needed = unsafe { sceMp3CheckStreamDataNeeded(instance.handle) };
    if needed > 0 {
        fill_stream_buffer(&mut instance.stream, instance.handle)?;
    }

    let mut buf: *mut i16 = ptr::null_mut();
    let bytes_decoded = unsafe { sceMp3Decode(instance.handle, &mut buf) };

    if bytes_decoded < 0 {
        if bytes_decoded as u32 != 0x80671402 {
            instance.error = true;
            return Err(bytes_decoded);
        }
    }

    if bytes_decoded == 0 || bytes_decoded as u32 == 0x80671402 {
        instance.over = true;
        instance.paused = true;
        let _ = unsafe { sys::sceMp3ResetPlayPosition(instance.handle) };
        return Ok(true);
    }

    // compute simple peak level from decoded PCM (i16 samples)
    if !buf.is_null() && bytes_decoded > 0 {
        let sample_count = (bytes_decoded as usize) / core::mem::size_of::<i16>();
        if sample_count > 0 {
            let samples = unsafe { core::slice::from_raw_parts(buf as *const i16, sample_count) };
            let mut peak: i32 = 0;
            for &s in samples.iter() {
                let v = (s as i32).abs();
                if v > peak {
                    peak = v;
                }
            }
            // normalize to 0..100
            let lvl = (peak as i64 * 100 / i16::MAX as i64) as i32;
            if !instance.shared.is_null() {
                unsafe {
                    (&*instance.shared).set_level(lvl);
                }
            }
        }
    }

    let result = unsafe { sys::sceAudioSRCOutputBlocking(0x8000, buf as *mut c_void) };

    if result < 0 {
        instance.error = true;
        return Err(result);
    }

    instance.num_played += result;

    Ok(false)
}

/// Internal state for an MP3 playback instance
#[allow(dead_code)]
struct Mp3Instance {
    stream: AssetStream,
    handle: Mp3Handle,
    paused: bool,
    error: bool,
    over: bool,
    num_played: i32,
    sampling_rate: i32,
    num_channels: i32,
    max_sample: i32,
    shared: *mut SharedState,
}

/// Shared state between main thread and audio thread
struct SharedState {
    stop_requested: AtomicBool,
    finished: AtomicBool,
    error: AtomicBool,
    last_error: AtomicI32,
    level: AtomicI32,
}

impl SharedState {
    fn new() -> Self {
        Self {
            stop_requested: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            error: AtomicBool::new(false),
            last_error: AtomicI32::new(0),
            level: AtomicI32::new(0),
        }
    }

    fn set_error(&self, err: i32) {
        self.last_error.store(err, Ordering::Relaxed);
        self.error.store(true, Ordering::Relaxed);
        self.finished.store(true, Ordering::Relaxed);
    }

    fn set_level(&self, v: i32) {
        self.level.store(v, Ordering::Relaxed);
    }
}

/// Arguments passed to the audio thread
struct ThreadArgs {
    path: String,
    shared: *mut SharedState,
}

// ThreadArgs needs to be Send for passing to thread
unsafe impl Send for ThreadArgs {}

/// Audio thread entry point
extern "C" fn mp3_thread_main(_args: usize, argp: *mut c_void) -> i32 {
    // argp points to a copy of the pointer value that was passed to sceKernelStartThread
    let args_ptr = unsafe { *(argp as *const *mut ThreadArgs) };
    let args: Box<ThreadArgs> = unsafe { Box::from_raw(args_ptr) };
    let shared = unsafe { &*args.shared };

    let result = mp3_thread_inner(&args.path, shared);

    if let Err(e) = result {
        shared.set_error(e);
    }

    shared.finished.store(true, Ordering::Relaxed);

    unsafe {
        sys::sceKernelExitDeleteThread(0);
    }

    0
}

/// Inner playback logic for the audio thread
fn mp3_thread_inner(path: &str, shared: &SharedState) -> Result<(), i32> {
    const SCE_ERROR_MODULE_ALREADY_LOADED: i32 = 0x80111102u32 as i32;

    unsafe {
        let r = sys::sceUtilityLoadModule(sys::Module::AvCodec);
        if r < 0 && r != SCE_ERROR_MODULE_ALREADY_LOADED {
            return Err(r);
        }
        let r = sys::sceUtilityLoadModule(sys::Module::AvMp3);
        if r < 0 && r != SCE_ERROR_MODULE_ALREADY_LOADED {
            return Err(r);
        }
    }

    let mut stream = AssetStream::open(path)?;

    let file_end = stream.size()?;

    let stream_start = find_stream_start(&mut stream)?;

    let init_result = unsafe { sceMp3InitResource() };
    if init_result < 0 {
        return Err(init_result);
    }

    let mp3_buf = unsafe { &raw mut MP3_BUF.0 as *mut u8 as *mut c_void };
    let pcm_buf = unsafe { &raw mut PCM_BUF.0 as *mut u8 as *mut c_void };

    let mut init_arg = sys::SceMp3InitArg {
        mp3_stream_start: stream_start,
        unk1: 0,
        mp3_stream_end: file_end as u32,
        unk2: 0,
        mp3_buf,
        mp3_buf_size: MP3_BUF_SIZE as i32,
        pcm_buf,
        pcm_buf_size: PCM_BUF_SIZE as i32,
    };

    let handle_raw = unsafe { sceMp3ReserveMp3Handle(&mut init_arg) };
    if handle_raw < 0 {
        unsafe { sceMp3TermResource() };
        return Err(handle_raw);
    }
    let handle = Mp3Handle(handle_raw);

    fill_stream_buffer(&mut stream, handle)?;

    let init_status = unsafe { sceMp3Init(handle) };
    if init_status < 0 {
        unsafe {
            sceMp3ReleaseMp3Handle(handle);
            sceMp3TermResource();
        }
        return Err(init_status);
    }

    let _ = unsafe { sys::sceAudioSRCChRelease() };

    let _ = unsafe { sceMp3SetLoopNum(handle, 0) };

    let sampling_rate = unsafe { sys::sceMp3GetSamplingRate(handle) };
    let num_channels = unsafe { sys::sceMp3GetMp3ChannelNum(handle) };
    let max_sample = unsafe { sys::sceMp3GetMaxOutputSample(handle) };

    let freq: AudioOutputFrequency = unsafe { core::mem::transmute(sampling_rate) };
    let channel = unsafe { sys::sceAudioSRCChReserve(max_sample, freq, num_channels) };
    if channel < 0 {
        unsafe {
            sceMp3ReleaseMp3Handle(handle);
            sceMp3TermResource();
        }
        return Err(channel);
    }

    let mut instance = Mp3Instance {
        stream,
        handle,
        paused: false,
        error: false,
        over: false,
        num_played: 0,
        sampling_rate,
        num_channels,
        max_sample,
        shared: shared as *const _ as *mut SharedState,
    };

    while !instance.over && !instance.error {
        if shared.stop_requested.load(Ordering::Relaxed) {
            break;
        }

        unsafe { sys::sceKernelDelayThreadCB(5000) };

        match mp3_feed(&mut instance) {
            Ok(_) => {}
            Err(e) => {
                shared.set_error(e);
                break;
            }
        }
    }

    unsafe {
        for _ in 0..10 {
            if sys::sceAudioSRCChRelease() >= 0 {
                break;
            }
            sys::sceKernelDelayThreadCB(100);
        }

        sceMp3ReleaseMp3Handle(handle);
        sceMp3TermResource();
    }

    Ok(())
}

/// MP3 player that runs audio playback in a separate thread
pub struct Mp3Player {
    thid: sys::SceUid,
    shared: *mut SharedState,
}

impl Mp3Player {
    /// The path should be a PSP file path like "ms0:/PSP/GAME/Project/assets/music.mp3"
    pub fn open(path: &str) -> Result<Self, &'static str> {
        let shared = Box::new(SharedState::new());
        let shared_ptr = Box::into_raw(shared);

        let args = Box::new(ThreadArgs {
            path: String::from(path),
            shared: shared_ptr,
        });
        let args_ptr = Box::into_raw(args);

        let thid = unsafe {
            sys::sceKernelCreateThread(
                b"mp3_play_thread\0".as_ptr(),
                mp3_thread_main,
                0x1F,  // Priority 31, same as C code
                0x800, // 2KB stack, same as C code
                sys::ThreadAttributes::USER | sys::ThreadAttributes::VFPU,
                ptr::null_mut(),
            )
        };

        if thid.0 < 0 {
            unsafe {
                drop(Box::from_raw(args_ptr));
                drop(Box::from_raw(shared_ptr));
            }
            return Err("Failed to create audio thread");
        }

        let result = unsafe {
            sys::sceKernelStartThread(
                thid,
                core::mem::size_of::<*mut ThreadArgs>(),
                &args_ptr as *const _ as *mut c_void,
            )
        };

        if result < 0 {
            unsafe {
                let _ = sys::sceKernelDeleteThread(thid);
                drop(Box::from_raw(args_ptr));
                drop(Box::from_raw(shared_ptr));
            }
            return Err("Failed to start audio thread");
        }

        Ok(Self {
            thid,
            shared: shared_ptr,
        })
    }

    /// - Ok(true) if still playing
    /// - Ok(false) if playback finished
    /// - Err with error message if playback failed
    pub fn tick(&mut self) -> Result<bool, &'static str> {
        let shared = unsafe { &*self.shared };

        if shared.error.load(Ordering::Relaxed) {
            return Err("MP3 playback error");
        }

        Ok(!shared.finished.load(Ordering::Relaxed))
    }

    /// Returns last computed level 0..100
    pub fn level(&self) -> i32 {
        let shared = unsafe { &*self.shared };
        shared.level.load(Ordering::Relaxed)
    }

    /// Stop playback
    #[allow(dead_code)]
    pub fn stop(&mut self) {
        let shared = unsafe { &*self.shared };
        shared.stop_requested.store(true, Ordering::Relaxed);
    }
}

impl Drop for Mp3Player {
    fn drop(&mut self) {
        let shared = unsafe { &*self.shared };
        shared.stop_requested.store(true, Ordering::Relaxed);

        unsafe {
            let _ = sys::sceKernelWaitThreadEnd(self.thid, ptr::null_mut());
            let _ = sys::sceKernelDeleteThread(self.thid);

            drop(Box::from_raw(self.shared));
        }
    }
}
