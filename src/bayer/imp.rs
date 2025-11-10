use gst::glib;
use gst::glib::ffi::GFALSE;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::subclass::prelude::*;
use gst_base_sys as ffi;
use gst_base_sys::GstBaseTransformClass;
use gst_video::VideoFrameExt;
use gst_video::VideoFrameRef;
use opencv::prelude::*;
use opencv::{Result, highgui, imgproc, videoio};
use std::sync::LazyLock;

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "rsbayer2rgb",
        gst::DebugColorFlags::empty(),
        Some("Bayer to RGB converter"),
    )
});

#[derive(Default)]
pub struct RsBayer2Rgb {
    state: std::sync::Mutex<Option<State>>,
}

struct State {
    in_info: InputInfo,
    out_info: gst_video::VideoInfo,
    intermediate_rgb: Option<opencv::core::Mat>,
}

struct InputInfo {
    width: usize,
    height: usize,
    stride: usize,
}

impl RsBayer2Rgb {}

#[glib::object_subclass]
impl ObjectSubclass for RsBayer2Rgb {
    const NAME: &'static str = "GstRsBayer2Rgb";
    type Type = super::RsBayer2Rgb;
    type ParentType = gst_base::BaseTransform; // Changed from VideoFilter

    fn class_init(klass: &mut Self::Class) {
        unsafe {
            let base_transform_class = &mut *(klass as *mut _ as *mut ffi::GstBaseTransformClass);
            base_transform_class.get_unit_size = Some(get_unit_size_trampoline)
        }
    }
}

unsafe extern "C" fn get_unit_size_trampoline(
    _ptr: *mut ffi::GstBaseTransform,
    caps: *mut gst_sys::GstCaps,
    size: *mut usize,
) -> glib::ffi::gboolean {
    unsafe {
        let caps = gst::Caps::from_glib_borrow(caps);

        let Some(structure) = caps.structure(0) else {
            gst::warning!(CAT, "get_unit_size: no structure in caps");
            return glib::ffi::GFALSE;
        };

        let Ok(width) = structure.get::<i32>("width") else {
            gst::warning!(CAT, "get_unit_size: no width in caps");
            return glib::ffi::GFALSE;
        };

        let Ok(height) = structure.get::<i32>("height") else {
            gst::warning!(CAT, "get_unit_size: no height in caps");
            return glib::ffi::GFALSE;
        };

        let width = width as usize;
        let height = height as usize;
        let result = match structure.name().as_str() {
            "video/x-bayer" => {
                *size = 1 * height * width;
                glib::ffi::GTRUE
            }
            "video/x-raw" => {
                let Ok(format) = structure.get::<&str>("format") else {
                    gst::warning!(
                        CAT,
                        "Could not find format in structure {}",
                        structure.to_string()
                    );
                    return glib::ffi::GFALSE;
                };
                match format {
                    "RGB" | "BGR" => {
                        *size = 3 * height * width;
                        glib::ffi::GTRUE
                    }
                    "RGBA"  => {
                        *size = 4 * height * width;
                        glib::ffi::GTRUE
                    }
                    _ => {
                        gst::warning!(CAT, "{} matched nothing", format);
                        glib::ffi::GFALSE
                    }
                }
            }
            other => {
                gst::warning!(CAT, "{} matched nothing", other);
                glib::ffi::GFALSE
            }
        };

        return result;
    }
}

impl ObjectImpl for RsBayer2Rgb {}
impl GstObjectImpl for RsBayer2Rgb {}

impl ElementImpl for RsBayer2Rgb {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Bayer to RGB Converter",
                "Filter/Converter/Video",
                "Uses OpenCV to convert bayer formats to RGB/BGR formats",
                "Eric Bridgeford",
            )
        });
        Some(&*ELEMENT_METADATA)
    }
    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            let sink_caps = gst::Caps::builder("video/x-bayer")
                .field("format", "rggb")
                .field("width", gst::IntRange::new(1, i32::MAX))
                .field("height", gst::IntRange::new(1, i32::MAX))
                .field(
                    "framerate",
                    gst::FractionRange::new(
                        gst::Fraction::new(0, 1),
                        gst::Fraction::new(i32::MAX, 1),
                    ),
                )
                .build();

            let sink_pad_template = gst::PadTemplate::new(
                "sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Always,
                &sink_caps,
            )
            .unwrap();

            let src_caps = gst_video::VideoCapsBuilder::new()
                .format_list([
                    gst_video::VideoFormat::Rgb,
                    gst_video::VideoFormat::Bgr,
                    gst_video::VideoFormat::Rgba,
                ])
                .build();

            let src_pad_template = gst::PadTemplate::new(
                "src",
                gst::PadDirection::Src,
                gst::PadPresence::Always,
                &src_caps,
            )
            .unwrap();

            vec![src_pad_template, sink_pad_template]
        });

        PAD_TEMPLATES.as_ref()
    }
}

impl BaseTransformImpl for RsBayer2Rgb {
    const MODE: gst_base::subclass::BaseTransformMode =
        gst_base::subclass::BaseTransformMode::NeverInPlace;
    const PASSTHROUGH_ON_SAME_CAPS: bool = false;
    const TRANSFORM_IP_ON_PASSTHROUGH: bool = false;

    fn transform_caps(
        &self,
        direction: gst::PadDirection,
        caps: &gst::Caps,
        filter: Option<&gst::Caps>,
    ) -> Option<gst::Caps> {
        let other_caps = if direction == gst::PadDirection::Src {
            // Transform src caps to sink caps (RGB -> Bayer)
            let mut result = gst::Caps::new_empty();

            for s in caps.iter() {
                let width = s.get::<i32>("width").ok();
                let height = s.get::<i32>("height").ok();
                let framerate = s.get::<gst::Fraction>("framerate").ok();

                let mut new_s = gst::Structure::builder("video/x-bayer").field("format", "rggb");

                if let Some(w) = width {
                    new_s = new_s.field("width", w);
                }
                if let Some(h) = height {
                    new_s = new_s.field("height", h);
                }
                if let Some(fr) = framerate {
                    new_s = new_s.field("framerate", fr);
                }

                result.get_mut().unwrap().append_structure(new_s.build());
            }
            result
        } else {
            // Transform sink caps to src caps (Bayer -> RGB)
            let mut result = gst::Caps::new_empty();

            for s in caps.iter() {
                let width = s.get::<i32>("width").ok();
                let height = s.get::<i32>("height").ok();
                let framerate = s.get::<gst::Fraction>("framerate").ok();

                // Create RGB variants
                for format in [
                    gst_video::VideoFormat::Rgb,
                    gst_video::VideoFormat::Bgr,
                    gst_video::VideoFormat::Rgba,
                ] {
                    let mut new_s =
                        gst::Structure::builder("video/x-raw").field("format", format.to_str());

                    if let Some(w) = width {
                        new_s = new_s.field("width", w);
                    }
                    if let Some(h) = height {
                        new_s = new_s.field("height", h);
                    }
                    if let Some(fr) = framerate {
                        new_s = new_s.field("framerate", fr);
                    }

                    result.get_mut().unwrap().append_structure(new_s.build());
                }
            }
            result
        };

        gst::info!(
            CAT,
            imp = self,
            "Transformed caps from {} to {} in direction {:?}",
            caps,
            other_caps,
            direction
        );

        if let Some(filter) = filter {
            Some(filter.intersect_with_mode(&other_caps, gst::CapsIntersectMode::First))
        } else {
            Some(other_caps)
        }
    }

    fn set_caps(&self, incaps: &gst::Caps, outcaps: &gst::Caps) -> Result<(), gst::LoggableError> {
        gst::info!(CAT, imp = self, "Input caps: {}", incaps);
        gst::info!(CAT, imp = self, "Output caps: {}", outcaps);

        // Parse Bayer input caps manually (VideoInfo doesn't support Bayer)
        let s = incaps.structure(0).unwrap();
        let width =
            s.get::<i32>("width")
                .map_err(|_| gst::loggable_error!(CAT, "No width in caps"))? as usize;
        let height =
            s.get::<i32>("height")
                .map_err(|_| gst::loggable_error!(CAT, "No height in caps"))? as usize;

        // For Bayer, stride is typically width (1 byte per pixel) but may be padded
        // Use width as stride - GStreamer will pad if needed
        let stride = width;

        let in_info = InputInfo {
            width,
            height,
            stride,
        };
        // Parse RGB output caps using VideoInfo
        let out_info = gst_video::VideoInfo::from_caps(outcaps)
            .map_err(|_| gst::loggable_error!(CAT, "Failed to parse output caps"))?;

        gst::info!(
            CAT,
            imp = self,
            "Input: {}x{}, stride: {}",
            width,
            height,
            stride
        );
        gst::info!(
            CAT,
            imp = self,
            "Output: {:?}, stride: {}",
            out_info.format(),
            out_info.stride()[0]
        );

        *self.state.lock().unwrap() = Some(State {
            in_info,
            out_info,
            intermediate_rgb: None,
        });

        Ok(())
    }

    fn transform(
        &self,
        inbuf: &gst::Buffer,
        outbuf: &mut gst::BufferRef,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        let mut state_guard = self.state.lock().unwrap();
        let state = state_guard.as_mut().ok_or(gst::FlowError::NotNegotiated)?;

        let in_map = inbuf.map_readable().map_err(|_| gst::FlowError::Error)?;
        let in_data = in_map.as_slice();

        let mut out_frame =
            gst_video::VideoFrameRef::from_buffer_ref_writable(outbuf, &state.out_info)
                .map_err(|_| gst::FlowError::Error)?;

        gst::info!(
            CAT,
            imp = self,
            "Transform: {}x{}, in_stride={}",
            state.in_info.width,
            state.in_info.height,
            state.in_info.stride,
        );

        match opencv_transform(&in_data, &mut out_frame, state) {
            Ok(()) => Ok(gst::FlowSuccess::Ok),
            Err(e) => Err(e),
        }
    }
}

fn opencv_transform(
    in_data: &[u8],
    out_frame: &mut gst_video::VideoFrameRef<&mut gst::BufferRef>,
    state: &mut State,
) -> Result<(), gst::FlowError> {
    let input_mat = unsafe {
        Mat::new_rows_cols_with_data_unsafe(
            state.in_info.height as i32,
            state.in_info.width as i32,
            opencv::core::CV_8UC1, //bayer will always be this
            in_data.as_ptr() as *mut std::ffi::c_void,
            state.in_info.stride,
        )
        .unwrap()
    };

    match state.out_info.format() {
        gst_video::VideoFormat::Bgr | gst_video::VideoFormat::Rgb =>
        //One pass, RGGB -> BGR/RGB
        {
            let conversion = match state.out_info.format() {
                gst_video::VideoFormat::Bgr => opencv::imgproc::COLOR_BayerBG2BGR,
                gst_video::VideoFormat::Rgb => opencv::imgproc::COLOR_BayerBG2RGB,
                _ => return Err(gst::FlowError::NotNegotiated),
            };
            let mut output_mat = unsafe {
                Mat::new_rows_cols_with_data_unsafe(
                    state.out_info.height() as i32,
                    state.out_info.width() as i32,
                    opencv::core::CV_8UC3, // Grayscale
                    out_frame.plane_data_mut(0).unwrap().as_mut_ptr() as *mut std::ffi::c_void,
                    out_frame.plane_stride()[0] as usize,
                )
                .unwrap()
            };
            // Process
            opencv::imgproc::cvt_color_def(&input_mat, &mut output_mat, conversion)
                .map(|_| ())
                .map_err(|_| gst::FlowError::Error)
        }
        gst_video::VideoFormat::Rgba => {
            //Two pass RGGB -> RGB -> RGBA, slow but more compatible

            //Put this first conversion on it's own bracket to limit the mutable scope of
            //intermdiate_rgb
            {
                let mut intermediate_rgb = match &mut state.intermediate_rgb {
                    Some(mat) => mat,
                    None => {
                        let mat = unsafe {
                            Mat::new_rows_cols(
                                state.in_info.height as i32,
                                state.in_info.width as i32,
                                opencv::core::CV_8UC3,
                            )
                            .unwrap()
                        };
                        state.intermediate_rgb = Some(mat);
                        state.intermediate_rgb.as_mut().unwrap()
                    }
                };

                opencv::imgproc::cvt_color_def(
                    &input_mat,
                    &mut intermediate_rgb,
                    opencv::imgproc::COLOR_BayerBG2RGB,
                )
                .map_err(|_| gst::FlowError::Error)?;
            }

            let mut output_mat = unsafe {
                Mat::new_rows_cols_with_data_unsafe(
                    state.out_info.height() as i32,
                    state.out_info.width() as i32,
                    opencv::core::CV_8UC4,
                    out_frame.plane_data_mut(0).unwrap().as_mut_ptr() as *mut std::ffi::c_void,
                    out_frame.plane_stride()[0] as usize,
                )
                .unwrap()
            };
            opencv::imgproc::cvt_color_def(
                state.intermediate_rgb.as_ref().unwrap(),
                &mut output_mat,
                opencv::imgproc::COLOR_RGB2RGBA,
            )
            .map(|_| ())
            .map_err(|_| gst::FlowError::Error)
        }
        _ => return Err(gst::FlowError::NotNegotiated),
    }
}
