use std::io::{Read, Write};
use std::path::Path;
use std::sync::Barrier;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_vm::{
    RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler, SchedulerConfig,
};

const WINDOW: usize = 16;
const MAX_INPUT: usize = 48 + 256 * 32;

#[derive(Clone, Copy, Debug)]
struct Request {
    actor: usize,
    sequence: i64,
    delta: i64,
}

struct Workload {
    actors: usize,
    bytes: usize,
    initial: i64,
    streams: [Vec<Request>; 2],
}

fn word(input: &[u8], index: usize) -> u64 {
    u64::from_be_bytes(
        input[index * 8..index * 8 + 8]
            .try_into()
            .expect("word range"),
    )
}

fn decode(input: &[u8]) -> Result<Workload, String> {
    if input.len() < 48 || input.len() > MAX_INPUT || word(input, 0) != 3 {
        return Err("header".into());
    }
    let bytes = usize::try_from(word(input, 1)).map_err(|_| "bytes")?;
    let actors = usize::try_from(word(input, 3)).map_err(|_| "actors")?;
    let counts = [word(input, 4), word(input, 5)];
    if ![256, 4096, 65536].contains(&bytes)
        || !(1..=100).contains(&actors)
        || counts.iter().any(|count| *count > 128)
        || input.len() != 48 + (counts[0] + counts[1]) as usize * 32
    {
        return Err("limits".into());
    }
    let initial = word(input, 2) as i64;
    let mut negative = vec![i128::from(initial); actors];
    let mut positive = negative.clone();
    let mut streams = [Vec::new(), Vec::new()];
    let mut offset = 6;
    for (stream, count) in streams.iter_mut().zip(counts) {
        let mut sequences = vec![0; actors];
        for _ in 0..count {
            let actor = usize::try_from(word(input, offset)).map_err(|_| "actor")?;
            let sequence = word(input, offset + 1);
            let delta = word(input, offset + 2) as i64;
            if !(1..=actors).contains(&actor)
                || sequence != sequences[actor - 1] + 1
                || word(input, offset + 3) != 0
            {
                return Err("request".into());
            }
            sequences[actor - 1] = sequence;
            negative[actor - 1] += i128::from(delta.min(0));
            positive[actor - 1] += i128::from(delta.max(0));
            stream.push(Request {
                actor,
                sequence: sequence as i64,
                delta,
            });
            offset += 4;
        }
    }
    if negative.iter().any(|value| *value < i128::from(i64::MIN))
        || positive.iter().any(|value| *value > i128::from(i64::MAX))
    {
        return Err("unsafe_interleaving".into());
    }
    Ok(Workload {
        actors,
        bytes,
        initial,
        streams,
    })
}

fn limits() -> Resources {
    Resources {
        bytes: 256 << 20,
        processes: 100,
        messages: 256,
        requests: 256,
    }
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    let duration = deadline.saturating_duration_since(Instant::now());
    if duration.is_zero() {
        Err("senders Counter deadline".into())
    } else {
        Ok(duration)
    }
}

fn execute(directory: &Path, workload: &Workload) -> Result<Value, String> {
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let tenant = vm.tenant_accounted(limits())?;
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: workload.actors,
            requests: 32,
            ..SchedulerConfig::default()
        },
    )?;
    let handle = scheduler.handle();
    let configure = module.handler("Configure")?;
    let apply = module.handler("Apply")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut pids = Vec::with_capacity(workload.actors);
    for identity in 1..=workload.actors {
        let pid = handle.spawn_with_heap(
            &module,
            Resources {
                bytes: 4 << 20,
                ..limits()
            },
            32,
            4,
        )?;
        let mut request = handle.request(
            pid,
            &configure,
            &[workload.bytes as i64, workload.initial, identity as i64],
            4096,
            remaining(deadline)?,
        )?;
        if request.wait(remaining(deadline)?)? != Some(RequestOutcome::Reply(workload.initial)) {
            return Err("senders Counter configure".into());
        }
        pids.push(pid);
    }
    let barrier = Barrier::new(2);
    let replies = std::thread::scope(|scope| {
        let mut senders = Vec::with_capacity(2);
        for (sender, stream) in workload.streams.iter().enumerate() {
            let handle = &handle;
            let pids = &pids;
            let apply = &apply;
            let barrier = &barrier;
            senders.push(scope.spawn(move || {
                barrier.wait();
                let mut replies = Vec::with_capacity(stream.len());
                for batch in stream.chunks(WINDOW) {
                    let mut pending = Vec::with_capacity(WINDOW);
                    for request in batch {
                        pending.push(handle.request(pids[request.actor - 1], apply,
                            &[sender as i64 + 1, request.sequence, request.delta, 0], 4096, remaining(deadline)?)?);
                    }
                    for (request, mut pending) in batch.iter().zip(pending) {
                        let Some(RequestOutcome::Reply(value)) = pending.wait(remaining(deadline)?)? else {
                            return Err::<Vec<Value>, String>("senders Counter unexpected terminal result".into());
                        };
                        replies.push(json!({"sender": sender + 1, "actor": request.actor, "seq": request.sequence, "value": value}));
                    }
                }
                Ok(replies)
            }));
        }
        let mut replies = Vec::with_capacity(256);
        for sender in senders {
            replies.extend(
                sender
                    .join()
                    .map_err(|_| "senders Counter host thread failed")??,
            );
        }
        Ok::<_, String>(replies)
    })?;
    let mut committed = Vec::with_capacity(workload.actors);
    for (index, pid) in pids.iter().enumerate() {
        remaining(deadline)?;
        let snapshot = handle
            .byte_state_snapshot(*pid)?
            .ok_or("missing byte state")?;
        if snapshot.length != workload.bytes
            || u64::from_be_bytes(snapshot.prefix[8..16].try_into().expect("identity"))
                != index as u64 + 1
        {
            return Err("senders Counter state layout".into());
        }
        for sender in 0..2 {
            let expected = workload.streams[sender]
                .iter()
                .filter(|request| request.actor == index + 1)
                .count() as i64;
            let offset = 16 + sender * 8;
            if i64::from_be_bytes(
                snapshot.prefix[offset..offset + 8]
                    .try_into()
                    .expect("sequence"),
            ) != expected
            {
                return Err("senders Counter final sequence".into());
            }
        }
        committed.push(i64::from_be_bytes(
            snapshot.prefix[..8].try_into().expect("counter"),
        ));
        handle.stop(*pid)?;
    }
    remaining(deadline)?;
    drop((scheduler, handle, configure, apply, module, tenant));
    if vm.used() != Resources::default() {
        return Err("senders Counter resource leak".into());
    }
    Ok(
        json!({"version": 3, "status": "ok", "actors": workload.actors, "state_bytes": workload.bytes, "message_bytes": 32,
        "committed": committed, "replies": replies, "states_verified": true, "resources_released": true,
        "environment": {"senders": 2, "window_per_sender": WINDOW, "workers": 2}}),
    )
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 || arguments[0] != "--trusted" {
        eprintln!("usage: beam_senders_counter --trusted ARTIFACT");
        std::process::exit(2);
    }
    let mut input = [0; MAX_INPUT + 1];
    let mut count = 0;
    let mut source = std::io::stdin().lock();
    while count < input.len() {
        match source.read(&mut input[count..]) {
            Ok(0) => break,
            Ok(size) => count += size,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => std::process::exit(2),
        }
    }
    let (code, output) = match decode(&input[..count]) {
        Ok(workload) => match execute(Path::new(&arguments[1]), &workload) {
            Ok(result) => (0, result),
            Err(error) => {
                eprintln!("{error}");
                (3, json!({"version": 3, "status": "oracle_error"}))
            }
        },
        Err(_) => (2, json!({"version": 3, "status": "invalid"})),
    };
    let _ = writeln!(std::io::stdout().lock(), "{output}");
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_rejects_bad_routes_sequences_limits_and_unsafe_interleavings() {
        let words = [3u64, 256, 0, 1, 1, 0, 1, 1, 7, 0];
        let input: Vec<_> = words.iter().flat_map(|word| word.to_be_bytes()).collect();
        assert_eq!(decode(&input).unwrap().streams[0][0].delta, 7);
        for (index, value) in [
            (0, 2),
            (1, 257),
            (3, 0),
            (3, 101),
            (4, 129),
            (6, 2),
            (7, 2),
            (9, 1),
            (2, i64::MAX as u64),
        ] {
            let mut changed = input.clone();
            changed[index * 8..index * 8 + 8].copy_from_slice(&value.to_be_bytes());
            assert!(decode(&changed).is_err());
        }
        for length in [0, 1, 47, 48, 79] {
            assert!(decode(&input[..length]).is_err());
        }
    }
}
