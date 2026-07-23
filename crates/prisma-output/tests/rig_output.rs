//! M2 harness, wire level: the same golden rig, lit one zone at a time, taken
//! all the way through the compiled [`OutputPlan`] to Art-Net channel data. This
//! proves the patch→wire binding on the real rig — color order applied, and the
//! isolation invariant holding on the actual DMX channels, not just the master
//! buffer.

use prisma_output::packet::artnet::parse_artdmx;
use prisma_output::plan::OutputPlan;
use prisma_output::router::OutputRouter;
use prisma_patch::{load_patch, Patch, PatchIndex};

// The golden rig lives with the patch crate; load the very same file here.
const GOLDEN: &str = include_str!("../../prisma-patch/tests/data/test_rig.patch.json");

const SOLID: [u8; 4] = [200, 150, 100, 50];

fn rig() -> (Patch, PatchIndex) {
    let patch = load_patch(GOLDEN).expect("golden rig loads");
    let index = patch.compile().expect("golden rig compiles");
    (patch, index)
}

fn light_zone(index: &PatchIndex, master: &mut [half::f16], slot: usize) {
    for &global in index.zone_members(slot) {
        let offset = index.pixel_component_offset(global) as usize;
        let n = index.pixel_component_count(global) as usize;
        for k in 0..n {
            master[offset + k] = half::f16::from_f32(SOLID[k] as f32 / 255.0);
        }
    }
}

#[test]
fn lighting_sector_left_reaches_the_wire_in_grb_order_and_isolated() {
    let (patch, index) = rig();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let mut router = OutputRouter::from_plan(plan);

    // One universe in this rig.
    assert_eq!(router.port_addresses(), vec![1]);

    // Light only sector-left: tube-a pixels 0 and 1 (GRB), at channels 1 and 4.
    let mut master = vec![half::f16::ZERO; index.master_buffer_len()];
    light_zone(&index, &mut master, 0);

    let pkt = router
        .encode_universe(0, &master, index.generation())
        .unwrap()
        .to_vec();
    let dmx = parse_artdmx(&pkt).unwrap();

    // Canonical colour is [R,G,B] = [200,150,100]; GRB reorders to [G,R,B].
    let grb = [SOLID[1], SOLID[0], SOLID[2]]; // [150, 200, 100]
    assert_eq!(&dmx.data[0..3], &grb, "pixel 0 wire order");
    assert_eq!(&dmx.data[3..6], &grb, "pixel 1 wire order");

    // Everything from channel 7 on belongs to sector-right (tube-a 2,3) and
    // tube-b — none of it was lit. Wire-level isolation.
    assert!(
        dmx.data[6..].iter().all(|&b| b == 0),
        "channels outside sector-left must be dark on the wire"
    );
}

#[test]
fn lighting_sector_b_applies_gamma_and_leaves_tube_a_dark() {
    let (patch, index) = rig();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let mut router = OutputRouter::from_plan(plan);

    // Light only sector-b: tube-b RGBW pixels at channels 20,24,28,32, gamma 2.2.
    let mut master = vec![half::f16::ZERO; index.master_buffer_len()];
    light_zone(&index, &mut master, 2);

    let pkt = router
        .encode_universe(0, &master, index.generation())
        .unwrap()
        .to_vec();
    let dmx = parse_artdmx(&pkt).unwrap();

    // Tube A (channels 1..=12) is untouched, and channels 13..=19 are the gap
    // before tube B starts — all dark on the wire.
    assert!(
        dmx.data[0..19].iter().all(|&b| b == 0),
        "tube A and the gap before tube B must stay dark when only sector-b is lit"
    );

    // Tube B pixel 0 R is channel 20 (index 19). gamma 2.2 darkens a mid value:
    // 200 → round(255*(200/255)^2.2) = 149 (well away from a rounding boundary),
    // strictly between 0 and the raw 200 — proof the LUT ran, not identity.
    let encoded_r = dmx.data[19];
    assert!(
        encoded_r > 0 && encoded_r < SOLID[0],
        "gamma should darken R=200, got {encoded_r}"
    );
    assert_eq!(encoded_r, 149, "gamma 2.2 on R=200");
}
