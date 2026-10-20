//! Built-in profile: SC024K HPA sniper rifle, learned from SC024K_0033.MP4
//! at 38.03 s and tuned on footage from 13/06/2026-11/10/2026.

use crate::audio::{ENV_FRAMES, N_BANDS, Signature};
use crate::profile::{Launch, Profile};
use crate::settings::DetectParams;

const SOURCE: &str = "SC024K_0033.MP4 @ 38s";
const PEAK: f32 = 0.49113193;
const HF_RATIO: f32 = 0.18901868;
#[rustfmt::skip]
const SPECTRUM: [f32; N_BANDS] = [-9.658688, -0.3923615, -9.658688, 0.48384738, -9.658688, 0.6994797, 1.2885224, 1.2712431, 0.7905229, 0.3976933, 0.45012197, 0.08511409, 1.1581635, 1.7335941, 1.164416, 0.6557255, 0.7456181, 1.2004832, 0.49064526, 1.6306607, 1.4300143, 1.2510873, 1.5365276, 1.046258, 1.117572, 1.3959383, 0.80445987, 0.80045533, 0.85828567, 0.9104593, 0.009620935, -0.10194984, -1.1579491, 1.2465607, 1.027094, 0.9931474, 0.7801976, 0.9233868, 0.3969728, -0.14556757];
#[rustfmt::skip]
const ENVELOPE: [f32; ENV_FRAMES] = [0.0, -0.52051497, -0.8578199, -0.78505766, -0.8240087, -0.7750733, -0.8810034, -1.127066, -0.85902905, -0.9807751, -1.2213284, -1.3335449, -1.2882026, -1.1589868, -1.2848128, -1.3274554, -1.555072, -1.4476, -1.5075215, -1.3801056, -1.4560269, -1.6279837, -1.6130456, -1.7060896, -1.6179258, -1.7295102, -1.7220939, -1.8679913, -1.8554155, -1.7705146, -1.7664028, -1.9006401, -1.8177654, -1.7976485, -1.9599983, -1.869116, -1.9404725, -1.8496963, -1.8257724, -2.003294];

pub fn profile() -> Profile {
    Profile {
        name: format!("Built-in: SC024K HPA sniper rifle ({SOURCE})"),
        signature: Signature {
            spectrum: SPECTRUM.to_vec(),
            envelope: ENVELOPE.to_vec(),
            peak: PEAK,
            hf_ratio: HF_RATIO,
        },
        detect: DetectParams::default(),
        launch: Some(Launch { x: 0.468, y: 0.465, radius: 0.055, dir_deg: -90.0, dir_tol_deg: 45.0 }),
    }
}
