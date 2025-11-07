use gst::glib;
use gst::prelude::*;

mod imp;

glib::wrapper! {
    pub struct RsBayer2Rgb(ObjectSubclass<imp::RsBayer2Rgb>)
        @extends gst_video::VideoFilter, gst_base::BaseTransform, gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "rsbayer2rgb",
        gst::Rank::NONE,
        RsBayer2Rgb::static_type(),
    )
}
