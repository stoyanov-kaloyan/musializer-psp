// this is basically copied over from musializere to the best of my ability

use core::f32::consts::PI;
extern crate alloc;
use alloc::{boxed::Box, vec};
use libm;

pub const FFT_SIZE: usize = 1 << 13; // 8192

pub struct Analyzer {
    pub in_raw: Box<[f32]>,
    pub in_win: Box<[f32]>,
    pub out_re: Box<[f32]>,
    pub out_im: Box<[f32]>,
    pub out_log: Box<[f32]>,
    pub out_smooth: Box<[f32]>,
    pub out_smear: Box<[f32]>,
}

impl Analyzer {
    pub fn new() -> Self {
        let zeros = vec![0.0f32; FFT_SIZE].into_boxed_slice();
        Self {
            in_raw: zeros.clone(),
            in_win: zeros.clone(),
            out_re: zeros.clone(),
            out_im: zeros.clone(),
            out_log: vec![0.0f32; FFT_SIZE / 2].into_boxed_slice(),
            out_smooth: vec![0.0f32; FFT_SIZE / 2].into_boxed_slice(),
            out_smear: vec![0.0f32; FFT_SIZE / 2].into_boxed_slice(),
        }
    }

    pub fn analyze(&mut self, samples: &[f32], dt: f32) -> usize {
        assert!(samples.len() == FFT_SIZE);

        // copy raw
        for i in 0..FFT_SIZE {
            self.in_raw[i] = samples[i];
        }

        // Apply Hann window
        for i in 0..FFT_SIZE {
            let t = (i as f32) / ((FFT_SIZE - 1) as f32);
            let hann = 0.5 - 0.5 * libm::cosf(2.0 * PI * t);
            self.in_win[i] = self.in_raw[i] * hann;
        }

        // prepare real/imag arrays
        for i in 0..FFT_SIZE {
            self.out_re[i] = self.in_win[i];
            self.out_im[i] = 0.0;
        }

        fft_inplace(&mut self.out_re, &mut self.out_im);

        // logarithmic squash
        let step: f32 = 1.06;
        let mut m: usize = 0;
        let mut max_amp: f32 = 1.0;
        let lowf: f32 = 1.0;
        let half = FFT_SIZE / 2;
        let mut f: f32 = lowf;
        while (f as usize) < half {
            let f1 = libm::ceilf(f * step);
            let mut a: f32 = 0.0;
            let start = f as usize;
            let end = f1 as usize;
            for q in start..end.min(half) {
                let val = amp(self.out_re[q], self.out_im[q]);
                if val > a {
                    a = val;
                }
            }
            if a > max_amp {
                max_amp = a;
            }
            if m < self.out_log.len() {
                self.out_log[m] = a;
            }
            m += 1;
            f = f1;
        }

        // normalize
        if max_amp > 0.0 {
            for i in 0..m {
                self.out_log[i] /= max_amp;
            }
        }

        // smoothing and smear
        for i in 0..m {
            let smoothness = 8.0f32;
            let smearness = 3.0f32;
            self.out_smooth[i] += (self.out_log[i] - self.out_smooth[i]) * smoothness * dt;
            self.out_smear[i] += (self.out_smooth[i] - self.out_smear[i]) * smearness * dt;
        }

        m
    }
}

fn amp(re: f32, im: f32) -> f32 {
    let a = re;
    let b = im;
    libm::logf(a * a + b * b)
}

fn fft_inplace(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    // bit reversal
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2usize;
    while len <= n {
        let ang = 2.0 * PI / (len as f32);
        let wlen_re = libm::cosf(ang);
        let wlen_im = libm::sinf(ang);
        let half = len / 2;
        let mut i = 0usize;
        while i < n {
            let mut w_re = 1.0f32;
            let mut w_im = 0.0f32;
            for j in 0..half {
                let u_re = re[i + j];
                let u_im = im[i + j];
                let v_re = re[i + j + half] * w_re - im[i + j + half] * w_im;
                let v_im = re[i + j + half] * w_im + im[i + j + half] * w_re;
                re[i + j] = u_re + v_re;
                im[i + j] = u_im + v_im;
                re[i + j + half] = u_re - v_re;
                im[i + j + half] = u_im - v_im;
                // w *= wlen
                let tmp = w_re * wlen_re - w_im * wlen_im;
                w_im = w_re * wlen_im + w_im * wlen_re;
                w_re = tmp;
            }
            i += len;
        }
        len <<= 1;
    }
}
