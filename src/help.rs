//! Long-form explanations shown when a setting's ℹ button is clicked.

// Gun profile
pub const PROFILE_LEARN: &str = "Before analysing, GhillieClip listens to the selected footage, groups the loudest repeated sounds and works out which group is your gun: the one whose sounds are followed by a BB leaving a consistent point in the view. From that it sets the shot sound, loudness and BB launch point.\n\nBest for a new gun, a new camera mount, or when the built-in profile misses shots. Needs a few clear shots in the footage.";
pub const PROFILE_BUILTIN: &str = "Uses the profile shipped with GhillieClip, learned from an SC024K HPA sniper rifle with a scope-mounted camera.\n\nSkips the learning step, so analysis starts immediately. Only accurate for a similar gun and mount.";
pub const PROFILE_FILE: &str = "Uses a profile you saved earlier with “Save profile…”.\n\nLearn once from good footage, save it, then reuse it for later sessions with the same gun and camera mount. Faster and more consistent than relearning every time.";
pub const PROFILE_SAVE: &str = "Saves the profile currently in effect (learned, loaded or built in) to a JSON file, so you can choose it later under “Saved profile”.";

// Clip timing
pub const PRE_SECS: &str = "How much footage is kept before each shot.\n\nLonger shows the aim and lead-up; shorter makes a tighter reel. Shots closer together than the combined before/after time are merged into one clip.";
pub const POST_SECS: &str = "How much footage is kept after each shot.\n\nMake it long enough to see the BB land and the reaction. Shots closer together than the combined before/after time are merged into one clip.";

// Shot detection
pub const THRESHOLD: &str = "How closely a sound's spectrum and decay must match the profile's shot sound, from 0 (anything) to 1 (identical).\n\nLower it if real shots are missed; raise it if other noises (impacts, other guns, wind) are counted as shots.";
pub const MIN_REL_LEVEL: &str = "Minimum loudness, relative to a typical shot from your gun.\n\nYour own gun is right next to the camera, so it is much louder than other players' guns. Raise this to ignore other players; lower it if quiet shots of yours are missed.";
pub const MAX_HF: &str = "Maximum share of the sound's energy above 4 kHz.\n\nBolt pulls, cocking and magazine clicks are sharp and high-pitched; a shot has more low-frequency energy. Lower this to reject more clicks; raise it if real shots are being rejected.";
pub const MIN_SHOT_GAP: &str = "Sounds closer together than this count as a single shot.\n\nPrevents echoes and reverberation from being counted twice. Lower it for full-auto or rapid follow-up shots; raise it in echoey places such as indoor sites.";
pub const ONSET_RATIO: &str = "How sharply the sound energy must jump above the background to mark the start of a new sound.\n\nHigher ignores softer, slower sounds; lower picks up shots in loud surroundings. The firing time is taken from this onset.";
pub const REQUIRE_BB: &str = "A shot sound only counts if a BB is found leaving the gun and is still visible at least this long after the shot.\n\nThis is the strongest filter against false shots. Set to 0 to accept every matching sound, for example when the BB can't be seen (dark footage, tracers off, wrong angle).";

// BB highlight & tracking
pub const HIGHLIGHT: &str = "Draws a coloured disc over the BB on every frame it is tracked, so viewers can follow it in flight.";
pub const HIGHLIGHT_SIZE: &str = "Diameter of the highlight disc as a percentage of the BB's measured on-screen size. 100 % covers the BB exactly; 200 % is twice its size.\n\nA bigger disc is easier to follow on a phone screen.";
pub const HIGHLIGHT_COLOUR: &str = "Colour of the highlight disc. Pick something that stands out against your field: orange or magenta works well on foliage.";
pub const HIGHLIGHT_MIN_RADIUS: &str = "Smallest the highlight disc may get, in pixels.\n\nA distant BB is only a pixel or two across; this keeps it visible as it shrinks.";
pub const SENSITIVITY: &str = "One slider for how hard the tracker tries to keep following the BB.\n\nLow: accepts only strong detections close to the predicted path. Tracks are reliable but may end early when the BB fades.\nHigh: accepts faint spots over a wider area. Tracks last longer but may wander onto leaves, players or sky clutter.\n\nMoving it resets the follow-phase values under Advanced tracking.";
pub const MAX_FLIGHT: &str = "How long after the shot to keep tracking the BB.\n\nLonger costs more processing time. Set it a little above your longest expected flight.";

// Advanced: find phase
pub const FIND_CONTRAST: &str = "How much brighter than its immediate surroundings a spot must be (in 8-bit brightness levels) to be accepted as the BB as it leaves the gun.\n\nLower finds the BB against bright backgrounds; higher avoids false starts on glints.";
pub const FIND_STATIC_RATIO: &str = "Rejects candidate spots that were already this visible (as a fraction of their contrast) before the shot.\n\nStops the tracker starting on a fixed bright object such as a light or a reflection.";
pub const FIND_RING_CHANGE: &str = "Maximum change around a candidate spot compared with before the shot, as a fraction of its contrast.\n\nIf the area around the spot changed a lot, it is probably a moving player or swaying foliage rather than a BB.";
pub const FIND_SEED: &str = "The BB must first be seen within this time after the shot.\n\nIncrease it if there is a delay between the sound and the BB appearing (slow frame rate, long barrel).";
pub const FIND_LATE: &str = "If no BB is seen leaving the launch point, accept one first seen anywhere up to this long after the shot.\n\nUseful when the BB is hidden at first, for example behind a bush or a window frame. 0 turns this off.";
pub const FIND_LATE_RADIUS: &str = "A late-found BB must first appear within this distance of the launch point, as a fraction of the frame width.\n\nKeeps late pick-ups near where the BB should be.";
pub const FIND_LATE_MIN: &str = "How many detections a late-found track needs before it is accepted.\n\nStricter than a normal track, because a BB not seen leaving the gun is more likely to be clutter.";

// Advanced: follow phase
pub const FOLLOW_CONTRAST: &str = "Minimum brightness over the surroundings a spot needs to continue an existing track.\n\nLower than the find-phase value, because the BB gets fainter as it travels. Lower follows further; higher stops before clutter.";
pub const FOLLOW_RADIUS: &str = "Base search radius in pixels around where the BB is predicted to be in the next frame.\n\nLarger tolerates recoil shake and prediction errors but is more likely to latch onto clutter.";
pub const FOLLOW_RADIUS_SPEED: &str = "Extra search radius for each pixel per frame of BB speed.\n\nA fast BB moves further between frames, so the search area grows with speed.";
pub const FOLLOW_MAX_GAP: &str = "How many frames in a row the BB may go undetected before the track ends.\n\nMissing frames are filled in by interpolation. Higher bridges brief occlusions (leaves, motion blur); lower ends tracks more cleanly.";
pub const FOLLOW_MAX_ACCEL: &str = "Largest change in the BB's on-screen velocity between frames, in pixels per frame².\n\nA real BB moves smoothly; a sudden change means it hit something or the tracker jumped to clutter.";
pub const FOLLOW_MAX_BRIGHTENING: &str = "A spot may be at most this many times brighter than recent BB detections.\n\nA BB only fades as it travels away, so a much brighter spot is something else.";
pub const FOLLOW_MIN_FADE: &str = "A spot must keep at least this fraction of the recent BB brightness.\n\nA sudden drop usually means a different, fainter object rather than the BB.";
pub const FOLLOW_REACQ_RADIUS: &str = "After the BB is missed in a frame, the search radius is multiplied by this when looking for it again.\n\nAbove 1 widens the search to recover the trail; below 1 is more cautious.";
pub const FOLLOW_REACQ_BRIGHTENING: &str = "After a missed frame, the spot that resumes the track may be at most this many times brighter than the last detection.\n\nStops a track resuming on an unrelated bright object.";
pub const FOLLOW_MAX_SPEED_GAIN: &str = "The BB's on-screen speed may grow to at most this multiple of its speed when following began (plus 3 px/frame).\n\nA BB slows down; a big speed-up means the tracker jumped elsewhere.";
pub const FOLLOW_NOVELTY: &str = "A spot must also be new compared with the frame from when the BB was this many pixels back along its path.\n\nRejects moving hands, players and foliage, which were already there before the BB passed.";
pub const FOLLOW_STATIC_RATIO: &str = "Rejects spots that were already this visible (as a fraction of their contrast) before the shot.\n\nStops the track jumping onto fixed bright objects along the BB's path.";
pub const FOLLOW_RING_CHANGE: &str = "Maximum change around a spot compared with before the shot, as a fraction of its contrast.\n\nRejects spots on moving players or swaying foliage.";

// Advanced: launch point
pub const LAUNCH_X: &str = "Horizontal position where the BB first appears, as a fraction of the frame width (0 = left edge, 1 = right edge).\n\nSet automatically when a profile is learned.";
pub const LAUNCH_Y: &str = "Vertical position where the BB first appears, as a fraction of the frame height (0 = top, 1 = bottom).\n\nSet automatically when a profile is learned.";
pub const LAUNCH_RADIUS: &str = "How far from the launch point a new track may start, as a fraction of the frame width.\n\nIncrease it if the camera mount moves between sessions.";
pub const LAUNCH_DIR: &str = "Direction the BB first moves on screen, in degrees: 0 = right, -90 = up, 90 = down, ±180 = left.\n\nSet automatically when a profile is learned.";
pub const LAUNCH_DIR_TOL: &str = "How far the BB's first movement may differ from the launch direction, in degrees.\n\nWider accepts more shots at odd angles but also more false starts.";

// Impact markers
pub const MARKERS: &str = "Draws a marker where an impact is detected: where the BB stops, dims, vanishes while still bright, or where the scene around it suddenly changes.";
pub const MARKER_STYLE: &str = "What the impact marker looks like:\n• Crosshair: a clean reticle.\n• Blood splatter: a stylised splat.\n• Explosion: an animated burst tinted by the marker colour.";
pub const MARKER_COLOUR: &str = "Main colour of the impact marker. The explosion style uses a fire palette tinted by this colour.";
pub const MARKER_SIZE: &str = "Diameter of the impact marker in pixels of the output video.";
pub const MARKER_DURATION: &str = "How long the impact marker stays on screen, in seconds.";
pub const IMPACT_STOP: &str = "Marks an impact where the BB suddenly slows to this fraction of its speed, meaning it hit something and stopped. Anything after that, such as the BB rolling down a blanket, is not drawn.\n\n0 turns this test off.";
pub const IMPACT_STOP_MIN: &str = "The sudden-stop test only applies if the BB was moving at least this fast (pixels per frame) before.\n\nDistant BBs slow gradually as they move away and should not count as stopping.";
pub const IMPACT_STOP_MAX: &str = "The sudden-stop test only applies if the BB's speed afterwards is at most this (pixels per frame), so it really stopped rather than slowing as it heads away.";
pub const IMPACT_DIM: &str = "Marks an impact where the BB's brightness suddenly drops to this fraction and stays there, typically when it hits a target and is shaded.\n\n0 turns this test off.";
pub const IMPACT_VANISH: &str = "Marks an impact if the BB disappears while still at least this many times brighter than the follow threshold.\n\nA bright BB that suddenly vanishes has usually hit something; a faint one has just faded with distance. Lower finds more impacts.";
pub const IMPACT_CHANGE: &str = "Marks an impact if the area around the BB's last position changes by at least this many brightness levels just after it vanishes (leaves moving, a target swinging, a player reacting).\n\nLower finds more impacts but may mark ones that did not happen.";

// Distance
pub const DIST_AT_IMPACT: &str = "Shows the estimated distance to the target above the impact point, calculated from the BB's flight time and the ballistics below.";
pub const DIST_LIVE: &str = "Shows the BB's current estimated distance next to it on every tracked frame, counting up as it flies.";
pub const DIST_UNITS: &str = "Units for the distance labels: metres or feet.";
pub const DIST_FONT: &str = "Height of the distance text in pixels of the output video.";
pub const DIST_COLOUR: &str = "Colour of the distance text. It is drawn with a dark outline so it stays readable on any background.";
pub const DIST_DECIMALS: &str = "Number of decimal places in the distance labels. 0 is usually right: the estimate is not accurate to fractions of a metre.";
pub const BB_MASS: &str = "Weight of the BBs you use, in grams (e.g. 0.20, 0.28, 0.40).\n\nHeavier BBs keep their speed for longer, so they travel further in the same time.";
pub const MUZZLE_FPS: &str = "Muzzle velocity in feet per second, as measured by a chronograph with the same BB weight.\n\nThe most important value for the distance estimate.";
pub const BB_DIAMETER: &str = "BB diameter in millimetres. Standard airsoft BBs are 5.95 mm; 8 mm BBs are used by some sniper rifles.";
pub const DRAG: &str = "Drag coefficient of the BB. About 0.47 for a smooth sphere; rougher or biodegradable BBs may be slightly higher.\n\nUsed with mass, diameter and air density to model how the BB slows.";
pub const AIR_DENSITY: &str = "Density of the air in kg/m³. 1.225 at sea level and 15 °C; lower at altitude or in hot weather (about 1.0 at 2,000 m).\n\nThinner air means less drag and slightly longer distances.";

// Encoding
pub const COPY: &str = "Cuts the clips without re-encoding the video.\n\nVery fast with no quality loss, but cuts snap to the nearest keyframe (so clips may start a little early) and nothing is drawn: no BB highlight, impact markers or distances.";
pub const CRF: &str = "Video quality (Constant Rate Factor) of the re-encoded reel. 0 is lossless, 18 is visually lossless, 23 is the usual default, 28+ is noticeably softer.\n\nLower means better quality and a larger file.";
pub const PRESET: &str = "How much effort the encoder spends compressing.\n\nSlower presets give a smaller file at the same quality but take longer. “veryfast” or “fast” is a good balance.";
pub const THREADS: &str = "How many shots are BB-tracked at the same time.\n\nMore is faster on a multi-core machine, but each uses about 0.5 GB of memory for 2.7K footage. Lower it if the computer runs out of memory.";
