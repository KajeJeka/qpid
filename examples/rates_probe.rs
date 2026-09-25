use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .expect("no default output device");
    println!("device: {:?}", device.name());
    let def = device.default_output_config().expect("no default config");
    println!(
        "default: {:?} {}Hz {}ch",
        def.sample_format(),
        def.config().sample_rate.0,
        def.config().channels
    );
    match device.supported_output_configs() {
        Ok(configs) => {
            let mut ranges: Vec<(u32, u32, u16, String)> = configs
                .map(|c| {
                    (
                        c.min_sample_rate().0,
                        c.max_sample_rate().0,
                        c.channels(),
                        format!("{:?}", c.sample_format()),
                    )
                })
                .collect();
            ranges.sort();
            ranges.dedup();
            for r in ranges {
                println!("range: {}-{} Hz, {}ch, {:?}", r.0, r.1, r.2, r.3);
            }
        }
        Err(e) => println!("supported_output_configs error: {e}"),
    }
}
