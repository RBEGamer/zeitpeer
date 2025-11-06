pub struct PairCalc {
    pub delay_ns: i128,
    pub offset_ns: i128,
}

pub fn compute_offset_delay(t1: i128, t2: i128, t3: i128, t4: i128) -> PairCalc {
    // classic four-timestamp equations
    let delay = (t4 - t1) - (t3 - t2);
    let offset = ((t2 - t1) + (t3 - t4)) / 2;
    PairCalc {
        delay_ns: delay,
        offset_ns: offset,
    }
}
