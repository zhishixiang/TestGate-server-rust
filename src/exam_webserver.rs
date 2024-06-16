use std::path::PathBuf;
use actix_files::NamedFile;
use actix_web::{get, post, Result, web, App, HttpResponse, HttpServer, Responder, HttpRequest};

#[get("/{test_id}")]
async fn index(path: web::Path<(String)>) -> impl Responder {
    let test_id = path.into_inner();
    HttpResponse::Ok().body(format!(" {test_id}"))
}
async fn resources(req: HttpRequest) -> Result<NamedFile> {
    let mut path:PathBuf = PathBuf::from("resources/");
    let filename:String = req.match_info().query("filename").parse().unwrap();
    path.push(filename);
    Ok(NamedFile::open(path)?)
}

pub fn new_actix_server(){
    let sys = actix_rt::System::new();
    sys.block_on(async {
        let server = HttpServer::new(|| {
            App::new()
                .service(index)
                .route("/resources/{filename:.*}",web::get().to(resources))
        })
            .bind("127.0.0.1:8081")
            .expect("HTTP服务无法绑定端口")
            .run();
        println!("HTTP服务启动成功");
        server.await.expect("HTTP服务意外退出:");
    });

    sys.run().expect("HTTP服务意外退出:");
}