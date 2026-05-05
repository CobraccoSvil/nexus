use redis::aio::MultiplexedConnection;
use redis::Client;

pub async fn init_redis(redis_url: &str) -> anyhow::Result<MultiplexedConnection> {
    let client = Client::open(redis_url)?;
    let conn = client.get_multiplexed_async_connection().await?;
    tracing::info!("Redis connection established");
    Ok(conn)
}
