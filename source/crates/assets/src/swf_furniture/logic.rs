//! The logic of the furni (Mapper/Logic: LogicMapper, MapPlanetSystem, MapParticleSystem, DataStructureLogic) and
//! the `logic` object SWF_Furni_To_Nitro builds from it.

use anyhow::Result;

use super::json::{Json, Obj};
use super::names::{parse_float, parse_int};
use super::xml::{element, elements};

/// A float? property (left out when null), through the float converter.
fn float(value: Option<f32>) -> Result<Option<Json>> {
    value.map(Json::float).transpose()
}

fn int(value: Option<i32>) -> Option<Json> {
    value.map(Json::int)
}

fn text(value: &Option<String>) -> Option<Json> {
    value.as_deref().map(Json::str)
}

fn attr(node: roxmltree::Node, name: &str) -> Option<String> {
    node.attribute(name).map(str::to_string)
}

pub struct Dimensions {
    x: f32,
    y: f32,
    z: f32,
    center_z: Option<f32>,
}

pub struct Action {
    link: Option<String>,
    start_state: Option<i32>,
}

pub struct PlanetSystem {
    id: Option<i32>,
    name: Option<String>,
    parent: Option<String>,
    radius: Option<f32>,
    arc_speed: Option<f32>,
    arc_offset: Option<f32>,
    blend: Option<i32>,
    height: Option<f32>,
}

pub struct Simulation {
    force: Option<f32>,
    direction: Option<f32>,
    gravity: Option<f32>,
    air_friction: Option<f32>,
    shape: Option<String>,
    energy: Option<f32>,
}

pub struct Particle {
    is_emitter: bool,
    life_time: Option<i32>,
    fade: bool,
    frames: Vec<String>,
}

pub struct Emitter {
    id: Option<i32>,
    name: Option<String>,
    sprite_id: Option<i32>,
    max_num_particles: Option<i32>,
    particles_per_frame: Option<i32>,
    burst_pulse: i32,
    fuse_time: Option<i32>,
    simulation: Option<Simulation>,
    particles: Option<Vec<Particle>>,
}

pub struct ParticleSystem {
    size: Option<i32>,
    canvas_id: Option<i32>,
    offset_y: Option<f32>,
    blend: Option<f32>,
    bg_color: Option<String>,
    emitters: Option<Vec<Emitter>>,
}

#[derive(Default)]
pub struct Logic {
    /// None without a <model>; else (dimensions, directions).
    model: Option<(Option<Dimensions>, Vec<i32>)>,
    action: Option<Action>,
    mask_type: Option<String>,
    credits: Option<String>,
    sound_sample: Option<(i32, bool)>,
    planet_systems: Vec<PlanetSystem>,
    particle_systems: Vec<ParticleSystem>,
    custom_vars: Vec<String>,
}

/// LogicMapper.MapLogicXml.
pub fn map(root: roxmltree::Node) -> Logic {
    let mut out = Logic::default();
    if let Some(model) = element(root, "model") {
        let dimensions = element(model, "dimensions").map(|d| {
            let f = |name: &str| parse_float(d.attribute(name)).filter(|v| !v.is_nan());
            Dimensions { x: f("x").unwrap_or(0.0), y: f("y").unwrap_or(0.0), z: f("z").unwrap_or(0.000001), center_z: f("centerZ") }
        });
        // MapDirections: no <directions> gives an empty list; one without valid ids gives [0].
        let directions = match element(model, "directions") {
            None => Vec::new(),
            Some(d) => {
                let mut ids: Vec<i32> = elements(d, "direction").filter_map(|e| parse_int(e.attribute("id"))).collect();
                if ids.is_empty() {
                    ids.push(0);
                }
                ids
            }
        };
        out.model = Some((dimensions, directions));
    }
    if let Some(action) = element(root, "action") {
        out.action = Some(Action { link: attr(action, "link"), start_state: parse_int(action.attribute("startState")) });
    }
    out.mask_type = element(root, "mask").and_then(|m| attr(m, "type"));
    out.credits = element(root, "credits").and_then(|m| attr(m, "value"));
    if let Some(sample) = element(root, "sound").and_then(|s| element(s, "sample")) {
        out.sound_sample = Some((parse_int(sample.attribute("id")).unwrap_or(0), sample.attribute("nopitch") == Some("true")));
    }
    if let Some(planets) = element(root, "planetsystem") {
        out.planet_systems = elements(planets, "object")
            .map(|p| PlanetSystem {
                id: parse_int(p.attribute("id")),
                name: attr(p, "name"),
                parent: attr(p, "parent"),
                radius: parse_float(p.attribute("radius")),
                arc_speed: parse_float(p.attribute("arcspeed")),
                arc_offset: parse_float(p.attribute("arcoffset")),
                blend: parse_int(p.attribute("blend")),
                height: parse_float(p.attribute("height")),
            })
            .collect();
    }
    if let Some(systems) = element(root, "particlesystems") {
        out.particle_systems = elements(systems, "particlesystem")
            .map(|p| {
                let emitters: Vec<Emitter> = elements(p, "emitter").map(map_emitter).collect();
                ParticleSystem {
                    size: parse_int(p.attribute("size")),
                    canvas_id: parse_int(p.attribute("canvas_id")),
                    offset_y: parse_float(p.attribute("offset_y")),
                    blend: parse_float(p.attribute("blend")),
                    bg_color: attr(p, "bg_color"),
                    emitters: (!emitters.is_empty()).then_some(emitters),
                }
            })
            .filter(|ps| ps.emitters.is_some() || ps.size.is_some())
            .collect();
    }
    if let Some(vars) = element(root, "customvars") {
        out.custom_vars = elements(vars, "variable").filter_map(|v| v.attribute("name")).filter(|n| !n.is_empty()).map(str::to_string).collect();
    }
    out
}

fn map_emitter(e: roxmltree::Node) -> Emitter {
    Emitter {
        id: parse_int(e.attribute("id")),
        name: attr(e, "name"),
        sprite_id: parse_int(e.attribute("sprite_id")),
        max_num_particles: parse_int(e.attribute("max_num_particles")),
        particles_per_frame: parse_int(e.attribute("particles_per_frame")),
        burst_pulse: parse_int(e.attribute("burst_pulse")).unwrap_or(1),
        fuse_time: parse_int(e.attribute("fuse_time")),
        simulation: element(e, "simulation").map(|s| Simulation {
            force: parse_float(s.attribute("force")),
            direction: parse_float(s.attribute("direction")),
            gravity: parse_float(s.attribute("gravity")),
            air_friction: parse_float(s.attribute("airfriction")),
            shape: attr(s, "shape"),
            energy: parse_float(s.attribute("energy")),
        }),
        particles: element(e, "particles").map(|ps| {
            elements(ps, "particle")
                .map(|p| Particle {
                    is_emitter: p.attribute("is_emitter") == Some("true"),
                    life_time: parse_int(p.attribute("lifetime")),
                    fade: p.attribute("fade") == Some("true"),
                    frames: elements(p, "frame").filter_map(|f| f.attribute("name")).filter(|n| !n.is_empty()).map(str::to_string).collect(),
                })
                .collect()
        }),
    }
}

fn emitter_json(e: &Emitter) -> Result<Json> {
    let mut o = Obj::new();
    o.opt("id", int(e.id))
        .opt("name", text(&e.name))
        .opt("spriteId", int(e.sprite_id))
        .opt("maxNumParticles", int(e.max_num_particles))
        .opt("particlesPerFrame", int(e.particles_per_frame))
        .put("burstPulse", Json::int(e.burst_pulse))
        .opt("fuseTime", int(e.fuse_time));
    if let Some(s) = &e.simulation {
        let mut so = Obj::new();
        so.opt("force", float(s.force)?)
            .opt("direction", float(s.direction)?)
            .opt("gravity", float(s.gravity)?)
            .opt("airFriction", float(s.air_friction)?)
            .opt("shape", text(&s.shape))
            .opt("energy", float(s.energy)?);
        o.put("simulation", so.build());
    }
    if let Some(particles) = &e.particles {
        let list = particles
            .iter()
            .map(|p| {
                Obj::new()
                    .put("isEmitter", Json::bool(p.is_emitter))
                    .opt("lifeTime", int(p.life_time))
                    .put("fade", Json::bool(p.fade))
                    .put("frames", Json::Arr(p.frames.iter().map(|f| Json::str(f)).collect()))
                    .build()
            })
            .collect();
        o.put("particles", Json::Arr(list));
    }
    Ok(o.build())
}

/// The anonymous `logicObject` of SWF_Furni_To_Nitro (its own property order, not DataStructureLogic's):
/// model { dimensions, directions }, action, maskType, credits, soundSample, planetSystems, particleSystems,
/// customVars, each left out when null; without a logic XML only `"model": {}` is left.
pub fn to_json(logic: Option<&Logic>) -> Result<Json> {
    let mut model = Obj::new();
    let mut o = Obj::new();
    let Some(logic) = logic else {
        return Ok(o.put("model", model.build()).build());
    };
    if let Some((dimensions, directions)) = &logic.model {
        if let Some(d) = dimensions {
            let mut dj = Obj::new();
            dj.put("x", Json::float(d.x)?).put("y", Json::float(d.y)?).put("z", Json::float(d.z)?).opt("centerZ", float(d.center_z)?);
            model.put("dimensions", dj.build());
        }
        // Model.Directions (not the serializable property): an empty list is written as [].
        model.put("directions", Json::Arr(directions.iter().map(|d| Json::int(*d)).collect()));
    }
    o.put("model", model.build());
    if let Some(a) = &logic.action {
        // LinkSerializable: an empty link is null.
        let link = a.link.as_deref().filter(|l| !l.is_empty()).map(Json::str);
        o.put("action", Obj::new().opt("link", link).opt("startState", int(a.start_state)).build());
    }
    o.opt("maskType", text(&logic.mask_type)).opt("credits", text(&logic.credits));
    if let Some((id, no_pitch)) = logic.sound_sample {
        o.put("soundSample", Obj::new().put("id", Json::int(id)).put("noPitch", Json::bool(no_pitch)).build());
    }
    if !logic.planet_systems.is_empty() {
        let mut list = Vec::new();
        for p in &logic.planet_systems {
            let mut po = Obj::new();
            po.opt("id", int(p.id))
                .opt("name", text(&p.name))
                .opt("parent", text(&p.parent))
                .opt("radius", float(p.radius)?)
                .opt("arcSpeed", float(p.arc_speed)?)
                .opt("arcOffset", float(p.arc_offset)?)
                .opt("blend", int(p.blend))
                .opt("height", float(p.height)?);
            list.push(po.build());
        }
        o.put("planetSystems", Json::Arr(list));
    }
    if !logic.particle_systems.is_empty() {
        let mut list = Vec::new();
        for p in &logic.particle_systems {
            let mut po = Obj::new();
            po.opt("size", int(p.size))
                .opt("canvasId", int(p.canvas_id))
                .opt("offsetY", float(p.offset_y)?)
                .opt("blend", float(p.blend)?)
                .opt("bgColor", text(&p.bg_color));
            if let Some(emitters) = &p.emitters {
                po.put("emitters", Json::Arr(emitters.iter().map(emitter_json).collect::<Result<_>>()?));
            }
            list.push(po.build());
        }
        o.put("particleSystems", Json::Arr(list));
    }
    if !logic.custom_vars.is_empty() {
        o.put("customVars", Obj::new().put("variables", Json::Arr(logic.custom_vars.iter().map(|v| Json::str(v)).collect())).build());
    }
    Ok(o.build())
}
