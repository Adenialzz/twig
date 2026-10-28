//! 将进程内采集时刻编码为 UTC 毫秒，客户端重建年龄而非误当成刚采集。
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub fn serialize<S: Serializer>(value: &Option<Instant>, serializer: S) -> Result<S::Ok, S::Error> {
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    value
        .map(|at| wall.saturating_sub(at.elapsed()).as_millis() as u64)
        .serialize(serializer)
}
pub fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Instant>, D::Error> {
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Ok(Option::<u64>::deserialize(deserializer)?
        .and_then(|ms| Instant::now().checked_sub(wall.saturating_sub(Duration::from_millis(ms)))))
}
