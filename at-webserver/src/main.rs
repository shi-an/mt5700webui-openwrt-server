use warp::Filter;

#[tokio::main]
async fn main() {
    env_logger::init();
    
    let hello = warp::path::end()
        .map(|| "Hello, World!");

    warp::serve(hello)
        .run(([127, 0, 0, 1], 3030))
        .await;
}
