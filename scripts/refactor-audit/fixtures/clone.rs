fn summarize(values: &[i64]) -> (i64, i64, i64) {
    let mut total = 0;
    let mut count = 0;
    let mut largest = 0;
    for value in values {
        if *value > 0 {
            total += *value;
            count += 1;
            if *value > largest {
                largest = *value;
            }
        }
    }
    let mean = if count > 0 { total / count } else { 0 };
    let mut deviation = 0;
    for value in values {
        let delta = *value - mean;
        if delta > 0 {
            deviation += delta;
        } else {
            deviation -= delta;
        }
    }
    let mut above = 0;
    let mut below = 0;
    for value in values {
        if *value > mean {
            above += 1;
        } else {
            below += 1;
        }
    }
    (total + deviation, count + above + below, largest)
}
