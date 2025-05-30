use crate::wcferry::{
    wcf::{
        AttachMsg, AudioMsg, DbNames, DbQuery, DbTable, DbTables, DecPath, ForwardMsg, MemberMgmt,
        MsgTypes, PatMsg, PathMsg, RichText, RpcContact, RpcContacts, TextMsg, Transfer, UserInfo,
        Verification,
    },
    SelfInfo, WeChat,
};
use base64::encode;
use log::{debug, error};
use reqwest::get;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::fs::File;
use std::io::{copy, Cursor};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;
use tokio::fs;
use utoipa::{IntoParams, OpenApi, ToSchema};
use utoipa_swagger_ui::Config;
use uuid::Uuid;
use warp::reply::Json;
use warp::{
    http::Uri,
    hyper::{Response, StatusCode},
    path::{FullPath, Tail},
    Filter, Rejection, Reply,
};

#[macro_export]
macro_rules! wechat_api_handler {
    ($wechat:expr, $handler:expr, $desc:expr) => {{
        let wechat = $wechat.lock().unwrap();
        let result: Result<_, _> = $handler(&*wechat);
        match result {
            Ok(data) => Ok(warp::reply::json(&ApiResponse {
                status: 0,
                error: None,
                data: Some(data),
            })),
            Err(error) => Ok(warp::reply::json(&ApiResponse::<()> {
                status: 1,
                error: Some(format!("{}失败: {}", $desc, error)),
                data: None,
            })),
        }
    }};
    ($wechat:expr, $handler:expr, $param:expr, $desc:expr) => {{
        let wechat = $wechat.lock().unwrap();
        let result: Result<_, _> = $handler(&*wechat, $param);
        match result {
            Ok(data) => Ok(warp::reply::json(&ApiResponse {
                status: 0,
                error: None,
                data: Some(data),
            })),
            Err(error) => Ok(warp::reply::json(&ApiResponse::<()> {
                status: 1,
                error: Some(format!("{}失败: {}", $desc, error)),
                data: None,
            })),
        }
    }};
}

#[macro_export]
macro_rules! build_route_fn {
    ($func_name:ident, GET $path:expr, $handler:expr, $wechat:expr) => {
        pub fn $func_name(
            wechat: Arc<Mutex<WeChat>>,
        ) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
            warp::path($path)
                .and(warp::get())
                .and(warp::any().map(move || wechat.clone()))
                .and_then($handler).boxed()
        }
    };
    ($func_name:ident, GET $path:expr, $handler:expr, PATH $param_type:ty, $wechat:expr) => {
        pub fn $func_name(
            wechat: Arc<Mutex<WeChat>>,
        ) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
            warp::path::param::<$param_type>()
                .and(warp::path($path))
                .and(warp::get())
                .and(warp::any().map(move || wechat.clone()))
                .and_then($handler).boxed()
        }
    };
    ($func_name:ident, GET $path:expr, $handler:expr, QUERY $param_type:ty, $wechat:expr) => {
        pub fn $func_name(
            wechat: Arc<Mutex<WeChat>>,
        ) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
            warp::path($path)
                .and(warp::get())
                .and(warp::query::<$param_type>())
                .and(warp::any().map(move || wechat.clone()))
                .and_then($handler).boxed()
        }
    };
    ($func_name:ident, POST $path:expr, $handler:expr, $wechat:expr) => {
        pub fn $func_name(
            wechat: Arc<Mutex<WeChat>>,
        ) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
            warp::path($path)
                .and(warp::post())
                .and(warp::any().map(move || wechat.clone()))
                .and_then($handler).boxed()
        }
    };
    ($func_name:ident, POST $path:expr, $handler:expr, QUERY $param_type:ty, $wechat:expr) => {
        pub fn $func_name(
            wechat: Arc<Mutex<WeChat>>,
        ) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
            warp::path($path)
                .and(warp::post())
                .and(warp::query::<$param_type>())
                .and(warp::any().map(move || wechat.clone()))
                .and_then($handler).boxed()
        }
    };
    ($func_name:ident, POST $path:expr, $handler:expr, JSON, $wechat:expr) => {
        pub fn $func_name(
            wechat: Arc<Mutex<WeChat>>,
        ) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
            warp::path($path)
                .and(warp::post())
                .and(warp::body::json())
                .and(warp::any().map(move || wechat.clone()))
                .and_then($handler).boxed()
        }
    };
}

#[derive(Serialize, ToSchema, Clone)]
#[aliases(ApiResponseBool = ApiResponse<bool>,
    ApiResponseString = ApiResponse<String>,
    ApiResponseUserInfo = ApiResponse<SelfInfo>,
    ApiResponseContacts = ApiResponse<RpcContacts>,
    ApiResponseDbNames = ApiResponse<DbNames>,
    ApiResponseMsgTypes = ApiResponse<MsgTypes>,
    ApiResponseDbTables = ApiResponse<DbTables>,
    ApiResponseMembers = ApiResponse<Vec<Member>>)]
struct ApiResponse<T>
where
    T: Serialize,
{
    status: u16,
    error: Option<String>,
    data: Option<T>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct Id {
    id: u64,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RoomId {
    #[serde(alias = "room_id")] // 兼容旧的 room_id
    roomid: String,
    #[serde(default)] // 允许字段缺失
    wxids: Option<String>, // 新增过滤字段
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct Image {
    /// 消息里的 id
    id: u64,
    /// 消息里的 extra
    extra: String,
    /// 存放目录，不存在则失败；没权限，亦失败
    #[schema(example = "C:/")]
    dir: String,
    /// 超时时间，单位秒
    #[schema(
        minimum = 0, 
        maximum = 255,
        format = "uint8",  // 可选：显式标记为 8 位无符号整数
        example = 10
    )]
    timeout: u8,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveFile {
    /// 消息里的 id
    id: u64,
    /// 消息里的 extra
    extra: String,
    thumb: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
#[serde(untagged)]
pub enum FieldContent {
    Int(i64),
    Float(f64),
    Utf8String(String),
    Base64String(String),
    None,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct Member {
    /// 微信ID
    pub wxid: String,
    /// 群内昵称
    pub name: String,
    pub state: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NewField {
    /// 字段名称
    pub column: String,
    /// 字段内容
    pub content: FieldContent,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NewRow {
    pub fields: Vec<NewField>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomMemberQuery {
    pub sql: String,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct DownloadImageParams {
    /// 消息里的 id
    id: u64,
    /// 消息里的 extra
    extra: String,
    /// 存放目录
    dir: String,
    /// 超时时间，单位秒
    #[schema(
        minimum = 0, 
        maximum = 255,
        format = "uint8",
        example = 10
    )]
    timeout: u8,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct DownloadFileParams {
    /// 消息里的 id
    id: u64,
    /// 消息里的 extra
    extra: String,
    /// 缩略图
    thumb: String,
}

pub fn get_routes(
    wechat: Arc<Mutex<WeChat>>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    let config = Arc::new(Config::from("/api-doc.json"));

    #[derive(OpenApi)]
    #[openapi(
        info(description = "<a href='https://github.com/lich0821/WeChatFerry'>WeChatFerry</a> 一个玩微信的工具。<table align='left'><tbody><tr><td align='center'><img width='160' alt='碲矿' src='https://s2.loli.net/2023/09/25/fub5VAPSa8srwyM.jpg'><div align='center' width='200'>后台回复 <code>WCF</code> 加群交流</div></td><td align='center'><img width='160' alt='赞赏' src='https://s2.loli.net/2023/09/25/gkh9uWZVOxzNPAX.jpg'><div align='center' width='200'>如果你觉得有用</div></td><td width='20%'></td><td width='20%'></td><td width='20%'></td></tr></tbody></table>"),
        paths(refresh_qrcode, is_login, get_self_wxid, get_user_info, get_contacts, get_dbs, get_tables, get_msg_types, save_audio,
            refresh_pyq, send_text, send_image, send_file, send_rich_text, send_pat_msg, forward_msg, save_image,save_file,
            recv_transfer, query_sql, accept_new_friend, add_chatroom_member, invite_chatroom_member,
            delete_chatroom_member, revoke_msg, query_room_member, download_image, download_file),
        components(schemas(
            ApiResponse<bool>, ApiResponse<String>, AttachMsg, AudioMsg, DbNames, DbQuery, DbTable, DbTables,
            DecPath, FieldContent, ForwardMsg, Image, SaveFile, MemberMgmt, MsgTypes, PatMsg, PathMsg, RichText, RpcContact,
            RpcContacts, TextMsg, Transfer, UserInfo, Verification, ApiResponse<Member>, Member, SelfInfo
        )),
        tags((name = "WCF", description = "玩微信的接口")),
    )]
    struct ApiDoc;

    let api_doc = warp::path("api-doc.json")
        .and(warp::get())
        .map(|| warp::reply::json(&ApiDoc::openapi()));

    let swagger_ui = warp::path("swagger")
        .and(warp::get())
        .and(warp::path::full())
        .and(warp::path::tail())
        .and(warp::any().map(move || config.clone()))
        .and_then(serve_swagger);
    
    build_route_fn!(qrcode, GET "qrcode", refresh_qrcode, wechat);
    build_route_fn!(islogin, GET "islogin", is_login, wechat);
    build_route_fn!(selfwxid, GET "selfwxid", get_self_wxid, wechat);
    build_route_fn!(userinfo, GET "userinfo", get_user_info, wechat);
    build_route_fn!(contacts, GET "contacts", get_contacts, wechat);
    build_route_fn!(dbs, GET "dbs", get_dbs, wechat);
    build_route_fn!(tables, GET "tables", get_tables, PATH String, wechat);
    build_route_fn!(msgtypes, GET "msg-types", get_msg_types, wechat);
    build_route_fn!(pyq, GET "pyq", refresh_pyq, QUERY Id, wechat);
    build_route_fn!(sendtext, POST "text", send_text, JSON, wechat);
    build_route_fn!(sendimage, POST "image", send_image, JSON, wechat);
    build_route_fn!(sendfile, POST "file", send_file, JSON, wechat);
    build_route_fn!(sendrichtext, POST "rich-text", send_rich_text, JSON, wechat);
    build_route_fn!(sendpatmsg, POST "pat", send_pat_msg, JSON, wechat);
    build_route_fn!(forwardmsg, POST "forward-msg", forward_msg, JSON, wechat);
    build_route_fn!(saveaudio, POST "audio", save_audio, JSON, wechat);
    build_route_fn!(saveimage, POST "save-image", save_image, JSON, wechat);
    build_route_fn!(savefile, POST "save-file", save_file, JSON, wechat);
    build_route_fn!(recvtransfer, POST "receive-transfer", recv_transfer, JSON, wechat);
    build_route_fn!(querysql, POST "sql", query_sql, JSON, wechat);
    build_route_fn!(acceptnewfriend, POST "accept-new-friend", accept_new_friend, JSON, wechat);
    build_route_fn!(addchatroommember, POST "add-chatroom-member", add_chatroom_member, JSON, wechat);
    build_route_fn!(invitechatroommember, POST "invite-chatroom-member", invite_chatroom_member, JSON, wechat);
    build_route_fn!(deletechatroommember, POST "delete-chatroom-member", delete_chatroom_member, JSON, wechat);
    build_route_fn!(revokemsg, POST "revoke-msg", revoke_msg, QUERY Id, wechat);
    build_route_fn!(queryroommember, GET "query-room-member", query_room_member, QUERY RoomId, wechat);
    build_route_fn!(downloadimage, GET "download-image", download_image, QUERY DownloadImageParams, wechat);
    build_route_fn!(downloadfile, GET "download-file", download_file, QUERY DownloadFileParams, wechat);

    api_doc
        .or(swagger_ui)
        .or(qrcode(wechat.clone()))
        .or(islogin(wechat.clone()))
        .or(selfwxid(wechat.clone()))
        .or(userinfo(wechat.clone()))
        .or(contacts(wechat.clone()))
        .or(dbs(wechat.clone()))
        .or(tables(wechat.clone()))
        .or(msgtypes(wechat.clone()))
        .or(pyq(wechat.clone()))
        .or(sendtext(wechat.clone()))
        .or(sendimage(wechat.clone()))
        .or(sendfile(wechat.clone()))
        .or(sendrichtext(wechat.clone()))
        .or(sendpatmsg(wechat.clone()))
        .or(forwardmsg(wechat.clone()))
        .or(saveaudio(wechat.clone()))
        .or(saveimage(wechat.clone()))
        .or(savefile(wechat.clone()))
        .or(recvtransfer(wechat.clone()))
        .or(querysql(wechat.clone()))
        .or(acceptnewfriend(wechat.clone()))
        .or(addchatroommember(wechat.clone()))
        .or(invitechatroommember(wechat.clone()))
        .or(deletechatroommember(wechat.clone()))
        .or(revokemsg(wechat.clone()))
        .or(queryroommember(wechat.clone()))
        .or(downloadimage(wechat.clone()))
        .or(downloadfile(wechat.clone()))
}

async fn serve_swagger(
    full_path: FullPath,
    tail: Tail,
    config: Arc<Config<'static>>,
) -> Result<Box<dyn Reply + 'static>, Rejection> {
    if full_path.as_str() == "/swagger" {
        return Ok(Box::new(warp::redirect::found(Uri::from_static(
            "/swagger/",
        ))));
    }

    let path = tail.as_str();
    match utoipa_swagger_ui::serve(path, config) {
        Ok(file) => {
            if let Some(file) = file {
                Ok(Box::new(
                    Response::builder()
                        .header("Content-Type", file.content_type)
                        .body(file.bytes),
                ))
            } else {
                Ok(Box::new(StatusCode::NOT_FOUND))
            }
        }
        Err(error) => Ok(Box::new(
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(error.to_string()),
        )),
    }
}

/// 获取登录二维码
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/qrcode",
    responses(
        (status = 200, body = ApiResponseString, description = "获取登录二维码")
    )
)]
pub async fn refresh_qrcode(wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::refresh_qrcode, "获取登录二维码")
}

/// 查询登录状态
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/islogin",
    responses(
        (status = 200, body = ApiResponseBool, description = "查询微信登录状态")
    )
)]
pub async fn is_login(wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::is_login, "查询微信登录状态")
}

/// 查询登录 wxid
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/selfwxid",
    responses(
        (status = 200, body = ApiResponseString, description = "返回登录账户 wxid")
    )
)]
pub async fn get_self_wxid(wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::get_self_wxid, "查询登录 wxid ")
}

/// 获取登录账号信息
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/userinfo",
    responses(
        (status = 200, body = ApiResponseUserInfo, description = "返回登录账户用户信息")
    )
)]
pub async fn get_user_info(wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::get_user_info, "获取登录账号信息")
}

/// 获取所有联系人
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/contacts",
    responses(
        (status = 200, body = ApiResponseContacts, description = "查询所有联系人，包括服务号、公众号、群聊等")
    )
)]
pub async fn get_contacts(wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::get_contacts, "获取所有联系人")
}

/// 获取所有可查询数据库
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/dbs",
    responses(
        (status = 200, body = ApiResponseDbNames, description = "查询所有可用数据库")
    )
)]
pub async fn get_dbs(wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::get_dbs, "获取所有可查询数据库")
}

/// 查询数据库下的表信息
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/{db}/tables",
    params(
        ("db" = String, Path, description = "目标数据库")
    ),
    responses(
        (status = 200, body = ApiResponseDbTables, description = "返回数据库表信息")
    )
)]
pub async fn get_tables(db: String, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::get_tables, db, "查询数据库下的表信息")
}

/// 获取消息类型枚举
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/msg-types",
    responses(
        (status = 200, body = ApiResponseMsgTypes, description = "返回消息类型")
    )
)]
pub async fn get_msg_types(wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::get_msg_types, "获取消息类型枚举")
}

/// 刷新朋友圈（在消息回调中查看）
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/pyq",
    params(("id"=u64, Query, description = "开始 id，0 为最新页")),
    responses(
        (status = 200, body = ApiResponseBool, description = "刷新朋友圈（从消息回调中查看）")
    )
)]
pub async fn refresh_pyq(query: Id, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::refresh_pyq, query.id, "刷新朋友圈")
}

/// 发送文本消息
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/text",
    request_body = TextMsg,
    responses(
        (status = 200, body = ApiResponseBool, description = "发送文本消息")
    )
)]
pub async fn send_text(text: TextMsg, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::send_text, text, "发送文本消息")
}

/// 发送图片
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/image",
    request_body = PathMsg,
    responses(
        (status = 200, body = ApiResponseBool, description = "发送图片消息")
    )
)]
pub async fn send_image(image: PathMsg, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    debug!("收到图片消息:\n{:?}", image);

    let mut image_path = PathBuf::from(image.path.clone());

    // 优先处理base64
    if !image.base64.is_empty() {
        let base64_data = &image.base64;
        debug!("检测到base64图片数据，开始解码");
        let extension = if image.path.ends_with(".jpg") || image.path.ends_with(".jpeg") {
            "jpg"
        } else if image.path.ends_with(".png") {
            "png"
        } else {
            "png"
        };
        let unique_filename = Uuid::new_v4().to_string();
        let local_image_path = PathBuf::from(format!("C:/images/{}.{}", unique_filename, extension));
        if let Err(e) = fs::create_dir_all(local_image_path.parent().unwrap()).await {
            debug!("创建目录失败: {:?}", e);
            return Ok(warp::reply::json(&json!({"error": "创建目录失败"})));
        }
        let decoded = match base64::decode(base64_data) {
            Ok(data) => data,
            Err(e) => {
                debug!("base64解码失败: {:?}", e);
                return Ok(warp::reply::json(&json!({"error": "base64解码失败"})));
            }
        };
        let mut file = match File::create(&local_image_path) {
            Ok(f) => f,
            Err(e) => {
                debug!("创建文件失败: {:?}", e);
                return Ok(warp::reply::json(&json!({"error": "创建文件失败"})));
            }
        };
        let mut cursor = Cursor::new(decoded);
        if let Err(e) = copy(&mut cursor, &mut file) {
            debug!("保存图片失败: {:?}", e);
            return Ok(warp::reply::json(&json!({"error": "保存图片失败"})));
        }
        debug!("base64图片保存成功, {:?}", local_image_path);
        image_path = PathBuf::from(local_image_path);
    } else if image.path.starts_with("http") {
        // 下载图片
        debug!("开始下载图片\n");
        let response = match get(&image.path).await {
            Ok(res) => res,
            Err(e) => {
                debug!("下载图片失败: {:?}", e);
                return Ok(warp::reply::json(&json!({"error": "下载图片失败"})));
            }
        };
        // 确认状态码
        debug!("响应状态码: {:?}", response.status());
        if response.status().is_success() {
            debug!("下载图片成功\n");
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|val| val.to_str().ok())
                .unwrap_or("image/png");
            let extension = match content_type {
                "image/jpeg" => "jpg",
                "image/png" => "png",
                _ => "png", // 默认使用png
            };

            // 使用 UUID 生成唯一的文件名
            let unique_filename = Uuid::new_v4().to_string();
            let local_image_path =
                PathBuf::from(format!("C:\\images\\{}.{}", unique_filename, extension));

            // 确保目录存在
            if let Err(e) = fs::create_dir_all(local_image_path.parent().unwrap()).await {
                debug!("创建目录失败: {:?}", e);
                return Ok(warp::reply::json(&json!({"error": "创建目录失败"})));
            }
            let mut file = match File::create(&local_image_path) {
                Ok(f) => f,
                Err(e) => {
                    debug!("创建文件失败: {:?}", e);
                    return Ok(warp::reply::json(&json!({"error": "创建文件失败"})));
                }
            };
            debug!("创建图片文件成功，开始获取图片内容做保存\n");
            // 获取图片内容并保存到文件
            let bytes = match response.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    debug!("读取图片内容失败: {:?}", e);
                    return Ok(warp::reply::json(&json!({"error": "读取图片内容失败"})));
                }
            };
            debug!("读取图片内容成功，开始保存图片内容\n");
            let mut cursor = Cursor::new(bytes);
            if let Err(e) = copy(&mut cursor, &mut file) {
                debug!("保存图片失败: {:?}", e);
                return Ok(warp::reply::json(&json!({"error": "保存图片失败"})));
            }
            debug!("保存图片内容成功, {:?}\n", local_image_path);
            image_path = PathBuf::from(local_image_path);
        } else {
            error!("下载图片失败，状态码: {:?}", response.status());
            return Ok(warp::reply::json(&json!({"error": "下载图片失败"})));
        }
    }

    // 更新 image 的路径
    let updated_image = PathMsg {
        path: image_path.to_string_lossy().to_string(),
        receiver: image.receiver,
        base64: String::new(),
    };

    wechat_api_handler!(wechat, WeChat::send_image, updated_image, "发送图片消息")
}

/// 发送文件
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/file",
    request_body = PathMsg,
    responses(
        (status = 200, body = ApiResponseBool, description = "发送文件消息")
    )
)]
pub async fn send_file(file: PathMsg, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::send_file, file, "发送文件消息")
}

/// 发送卡片消息
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/rich-text",
    request_body = RichText,
    responses(
        (status = 200, body = ApiResponseBool, description = "发送卡片消息")
    )
)]
pub async fn send_rich_text(msg: RichText, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::send_rich_text, msg, "发送卡片消息")
}

/// 拍一拍
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/pat",
    request_body = PatMsg,
    responses(
        (status = 200, body = ApiResponseBool, description = "发送拍一拍消息")
    )
)]
pub async fn send_pat_msg(msg: PatMsg, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::send_pat_msg, msg, "发送拍一拍消息")
}

/// 转发消息
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/forward-msg",
    request_body = ForwardMsg,
    responses(
        (status = 200, body = ApiResponseBool, description = "转发消息")
    )
)]
pub async fn forward_msg(msg: ForwardMsg, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::forward_msg, msg, "转发消息")
}

/// 保存语音
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/audio",
    request_body = AudioMsg,
    responses(
        (status = 200, body = ApiResponseString, description = "保存语音消息")
    )
)]
pub async fn save_audio(msg: AudioMsg, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::save_audio, msg, "保存语音")
}

/// 保存图片
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/save-image",
    request_body = Image,
    responses(
        (status = 200, body = ApiResponseString, description = "保存图片")
    )
)]
pub async fn save_image(msg: Image, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    let wc = wechat.lock().unwrap();
    let handle_error = |error_message: &str| -> Result<Json, Infallible> {
        Ok(warp::reply::json(&ApiResponse::<String> {
            status: 1,
            error: Some(error_message.to_string()),
            data: None,
        }))
    };

    let att = AttachMsg {
        id: msg.id,
        thumb: "".to_string(),
        extra: msg.extra.clone(),
    };

    let status = match wc.clone().download_attach(att) {
        Ok(status) => status,
        Err(error) => return handle_error(&error.to_string()),
    };

    if !status {
        return handle_error("下载失败");
    }

    let mut counter = 0;
    loop {
        if counter >= msg.timeout {
            break;
        }
        match wc.clone().decrypt_image(DecPath {
            src: msg.extra.clone(),
            dst: msg.dir.clone(),
        }) {
            Ok(path) => {
                if path.is_empty() {
                    counter += 1;
                    sleep(Duration::from_secs(1));
                    continue;
                }
                return Ok(warp::reply::json(&ApiResponse {
                    status: 0,
                    error: None,
                    data: Some(path),
                }));
            }
            Err(error) => return handle_error(&error.to_string()),
        };
    }
    return handle_error("下载超时");
}

/// 保存文件
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/save-file",
    request_body = SaveFile,
    responses(
        (status = 200, body = ApiResponseString, description = "保存文件(只下载不解密)")
    )
)]
pub async fn save_file(msg: SaveFile, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    let wc = wechat.lock().unwrap();
    let handle_error = |error_message: &str| -> Result<Json, Infallible> {
        Ok(warp::reply::json(&ApiResponse::<String> {
            status: 1,
            error: Some(error_message.to_string()),
            data: None,
        }))
    };

    let att = AttachMsg {
        id: msg.id,
        thumb: msg.thumb.to_string(),
        extra: msg.extra.clone(),
    };

    let status = match wc.clone().download_attach(att) {
        Ok(status) => status,
        Err(error) => return handle_error(&error.to_string()),
    };

    if !status {
        return handle_error("下载失败");
    }

    return Ok(warp::reply::json(&ApiResponse {
        status: 0,
        error: None,
        data: Some("ok".to_owned()),
    }));
}

/// 接收转账
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/receive-transfer",
    request_body = Transfer,
    responses(
        (status = 200, body = ApiResponseBool, description = "接收转账")
    )
)]
pub async fn recv_transfer(msg: Transfer, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::recv_transfer, msg, "接收转账")
}

/// 执行 SQL 查询数据库
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/sql",
    request_body = DbQuery,
    responses(
        (status = 200, body = Vec<HashMap<String, FieldContent>>, description = "执行 SQL")
    )
)]
pub async fn query_sql(msg: DbQuery, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    let wechat = wechat.lock().unwrap();
    let rsp = match wechat.clone().query_sql(msg) {
        Ok(origin) => {
            let rows = origin
                .rows
                .into_iter()
                .map(|r| {
                    let mut row_map = HashMap::new();
                    for f in r.fields {
                        let utf8 = String::from_utf8(f.content.clone()).unwrap_or_default();
                        let content: FieldContent = match f.r#type {
                            1 => utf8
                                .parse::<i64>()
                                .map_or(FieldContent::None, FieldContent::Int),
                            2 => utf8
                                .parse::<f64>()
                                .map_or(FieldContent::None, FieldContent::Float),
                            3 => FieldContent::Utf8String(utf8),
                            4 => FieldContent::Base64String(encode(&f.content.clone())),
                            _ => FieldContent::None,
                        };
                        row_map.insert(f.column, content);
                    }
                    row_map
                })
                .collect::<Vec<_>>();

            ApiResponse {
                status: 0,
                error: None,
                data: Some(rows),
            }
        }
        Err(error) => ApiResponse {
            status: 1,
            error: Some(error.to_string()),
            data: None,
        },
    };
    Ok(warp::reply::json(&rsp))
}

/// 通过好友申请
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/accept-new-friend",
    request_body = Verification,
    responses(
        (status = 200, body = ApiResponseBool, description = "通过好友申请")
    )
)]
pub async fn accept_new_friend(
    msg: Verification,
    wechat: Arc<Mutex<WeChat>>,
) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::accept_new_friend, msg, "通过好友申请")
}

/// 添加群成员
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/add-chatroom-member",
    request_body = MemberMgmt,
    responses(
        (status = 200, body = ApiResponseBool, description = "添加群成员")
    )
)]
pub async fn add_chatroom_member(
    msg: MemberMgmt,
    wechat: Arc<Mutex<WeChat>>,
) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::add_chatroom_member, msg, "添加群成员")
}

/// 邀请群成员
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/invite-chatroom-member",
    request_body = MemberMgmt,
    responses(
        (status = 200, body = ApiResponseBool, description = "邀请群成员")
    )
)]
pub async fn invite_chatroom_member(
    msg: MemberMgmt,
    wechat: Arc<Mutex<WeChat>>,
) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::invite_chatroom_member, msg, "邀请群成员")
}

/// 删除群成员（踢人）
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/delete-chatroom-member",
    request_body = MemberMgmt,
    responses(
        (status = 200, body = ApiResponseBool, description = "删除群成员")
    )
)]
pub async fn delete_chatroom_member(
    msg: MemberMgmt,
    wechat: Arc<Mutex<WeChat>>,
) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::delete_chatroom_member, msg, "删除群成员")
}

/// 撤回消息
#[utoipa::path(
    post,
    tag = "WCF",
    path = "/revoke-msg",
    params(("id"=u64, Query, description = "待撤回消息 id")),
    responses(
        (status = 200, body = ApiResponseBool, description = "撤回消息")
    )
)]
pub async fn revoke_msg(msg: Id, wechat: Arc<Mutex<WeChat>>) -> Result<Json, Infallible> {
    wechat_api_handler!(wechat, WeChat::revoke_msg, msg.id, "撤回消息")
}

/// 查询群成员
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/query-room-member",
    params(
        ("roomid" = String, Query, 
            example = "123@chatroom", 
            description = "群ID"),
        ("wxids" = Option<String>, Query, 
            example = "wxid_abc,wxid_def", 
            description = "可选-逗号分隔的成员微信ID列表")
    ),
    responses(
        (status = 200, body = ApiResponseMembers, description = "查询群成员")
    )
)]
pub async fn query_room_member(
    query: RoomId,
    wechat: Arc<Mutex<WeChat>>,
) -> Result<Json, Infallible> {
    let wechat = wechat.lock().unwrap();

        // 解析逗号分隔的wxid列表
    let target_ids: HashSet<String> = query.wxids
        .as_deref()
        .map(|s| s.split(','))
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    
    let resp = match wechat.clone().query_room_member(query.roomid.clone()) {
        Ok(members) => match members {
            Some(mbs) => {
                let filtered_members: Vec<_> = mbs
                    .into_iter()
                    .filter(|m| target_ids.is_empty() || target_ids.contains(&m.wxid))
                    .map(|member| Member {
                        wxid: member.wxid,
                        name: member.name,
                        state: member.state,
                    })
                    .collect();
                ApiResponse {
                    status: 0,
                    error: None,
                    data: Some(filtered_members),
                }
            }
            None => ApiResponse {
                status: 0,
                error: None,
                data: Some(vec![]),
            },
        },
        Err(e) => ApiResponse {
            status: 1,
            error: Some(e.to_string()),
            data: None,
        },
    };
    Ok(warp::reply::json(&resp))
}

/// 下载图片
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/download-image",
    params(
        ("id" = u64, Query, description = "消息ID"),
        ("extra" = String, Query, description = "extra"),
        ("dir" = String, Query, description = "存放目录"),
        ("timeout" = u8, Query, description = "超时时间(秒)")
    ),
    responses(
        (status = 200, description = "返回图片文件流", content_type = "image/*")
    )
)]
pub async fn download_image(params: DownloadImageParams, wechat: Arc<Mutex<WeChat>>) -> Result<Box<dyn Reply>, Infallible> {
    let handle_error = |error_message: String| -> Result<Box<dyn Reply>, Infallible> {
        Ok(Box::new(warp::reply::with_status(
            error_message,
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        )))
    };

    let att = AttachMsg {
        id: params.id,
        thumb: "".to_string(),
        extra: params.extra.clone(),
    };

    let status = {
        let wc = wechat.lock().unwrap();
        match wc.clone().download_attach(att) {
            Ok(status) => status,
            Err(error) => return handle_error(error.to_string()),
        }
    };

    if !status {
        return handle_error("下载失败".to_string());
    }

    let mut counter = 0;
    loop {
        if counter >= params.timeout {
            break;
        }
        let path = {
            let wc = wechat.lock().unwrap();
            match wc.clone().decrypt_image(DecPath {
                src: params.extra.clone(),
                dst: params.dir.clone(),
            }) {
                Ok(path) => path,
                Err(error) => return handle_error(error.to_string()),
            }
        };

        if path.is_empty() {
            counter += 1;
            sleep(Duration::from_secs(1));
            continue;
        }
        
        // 读取文件内容
        match tokio::fs::read(&path).await {
            Ok(content) => {
                // 根据文件扩展名确定 Content-Type
                let content_type = if path.ends_with(".jpg") || path.ends_with(".jpeg") {
                    "image/jpeg"
                } else if path.ends_with(".png") {
                    "image/png"
                } else {
                    "application/octet-stream"
                };

                // 返回文件流
                return Ok(Box::new(warp::reply::with_header(
                    content,
                    "Content-Type",
                    content_type,
                )));
            }
            Err(e) => return handle_error(format!("读取文件失败: {}", e)),
        }
    }
    return handle_error("下载超时".to_string());
}

/// 下载文件
#[utoipa::path(
    get,
    tag = "WCF",
    path = "/download-file",
    params(
        ("id" = u64, Query, description = "消息ID"),
        ("extra" = String, Query, description = "extra"),
        ("thumb" = String, Query, description = "缩略图")
    ),
    responses(
        (status = 200, description = "返回文件流", content_type = "application/octet-stream")
    )
)]
pub async fn download_file(params: DownloadFileParams, wechat: Arc<Mutex<WeChat>>) -> Result<Box<dyn Reply>, Infallible> {
    let handle_error = |error_message: String| -> Result<Box<dyn Reply>, Infallible> {
        Ok(Box::new(warp::reply::with_status(
            error_message,
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        )))
    };

    let att = AttachMsg {
        id: params.id,
        thumb: params.thumb,
        extra: params.extra.clone(),
    };

    let status = {
        let wc = wechat.lock().unwrap();
        match wc.clone().download_attach(att) {
            Ok(status) => status,
            Err(error) => return handle_error(error.to_string()),
        }
    };

    if !status {
        return handle_error("下载失败".to_string());
    }

    // 读取文件内容
    match tokio::fs::read(&params.extra).await {
        Ok(content) => {
            // 获取文件扩展名
            let extension = std::path::Path::new(&params.extra)
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("");

            // 根据文件扩展名确定 Content-Type
            let content_type = match extension.to_lowercase().as_str() {
                "pdf" => "application/pdf",
                "doc" | "docx" => "application/msword",
                "xls" | "xlsx" => "application/vnd.ms-excel",
                "ppt" | "pptx" => "application/vnd.ms-powerpoint",
                "zip" => "application/zip",
                "rar" => "application/x-rar-compressed",
                "txt" => "text/plain",
                "json" => "application/json",
                "xml" => "application/xml",
                "html" | "htm" => "text/html",
                "css" => "text/css",
                "js" => "application/javascript",
                "mp3" => "audio/mpeg",
                "mp4" => "video/mp4",
                "jpg" | "jpeg" => "image/jpeg",
                "png" => "image/png",
                "gif" => "image/gif",
                _ => "application/octet-stream",
            };

            // 返回文件流
            return Ok(Box::new(warp::reply::with_header(
                content,
                "Content-Type",
                content_type,
            )));
        }
        Err(e) => return handle_error(format!("读取文件失败: {}", e)),
    }
}
