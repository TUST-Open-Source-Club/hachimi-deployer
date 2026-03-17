# hachimi_deployer

一个使用 Rust + Axum 编写的轻量部署维护服务。

它用于接收远端 CI/CD 上传的 `docker save` 镜像 tar，通过 Docker 或 Podman 的 unix socket 导入镜像，并替换当前正在使用该镜像的容器。服务从 TOML 读取允许部署的镜像列表及各自独立的 Bearer Token，并输出结构化 tracing 日志，便于后续审计。

## 功能

- 提供 `PUT /deploy/{image_ref}` 部署接口
- 从 TOML 配置读取：
  - 服务监听地址
  - Docker / Podman 管理 socket
  - 允许部署的镜像名
  - 每个镜像独立的 Bearer Token
- 接收 `docker save` 生成的 tar 字节流
- 先将上传内容落盘到临时文件，再调用 engine `load`
- 只替换当前 `Config.Image` 精确匹配目标镜像名的容器
- 输出 JSON 格式 tracing 日志

## 工作方式

请求流程如下：

1. 客户端调用 `PUT /deploy/{percent-encoded image_ref}`
2. 服务端校验该镜像是否在配置中存在
3. 校验 `Authorization: Bearer <token>`
4. 将请求体保存为临时 tar 文件
5. 调用 Docker-compatible HTTP API 导入镜像
6. 查找所有当前使用该镜像名的容器
7. 对每个容器执行替换：
   - inspect 原容器
   - 使用新镜像创建替代容器
   - 停止并移除旧容器
   - 将新容器重命名为旧容器名
   - 启动新容器
8. 记录整次操作的 tracing 信息

## 配置

默认配置路径为 `config/deployer.toml`。

也可以通过环境变量指定：

```bash
export HACHIMI_CONFIG=/path/to/deployer.toml
```

配置示例：

```toml
[server]
listen = "0.0.0.0:3000"

[engine]
socket_path = "/var/run/docker.sock"

[[images]]
image_ref = "ghcr.io/acme/app:prod"
bearer_token = "replace-me"

[[images]]
image_ref = "docker.io/library/nginx:latest"
bearer_token = "another-token"
```

字段说明：

- `server.listen`: HTTP 服务监听地址
- `engine.socket_path`: Docker 或 Podman 的 unix socket 路径
- `images[].image_ref`: 允许部署的完整镜像名，必须精确到标签
- `images[].bearer_token`: 该镜像专用的 Bearer Token

约束：

- `image_ref` 不能为空
- `bearer_token` 不能为空
- 不允许重复的 `image_ref`
- 至少需要一个 `[[images]]`

## 运行

开发环境启动：

```bash
cargo run
```

如果使用默认配置路径，请先准备：

```bash
mkdir -p config
cp config/deployer.example.toml config/deployer.toml
```

测试：

```bash
cargo test
```

## API

### `PUT /deploy/{image_ref}`

说明：

- `image_ref` 必须是完整镜像名做 percent-encode 后的结果
- 请求头必须带 `Authorization: Bearer <token>`
- 请求体为 `docker save` 导出的原始 tar 流

示例镜像名：

```text
ghcr.io/acme/app:prod
```

percent-encode 后：

```text
ghcr.io%2Facme%2Fapp%3Aprod
```

调用示例：

```bash
docker save ghcr.io/acme/app:prod | \
curl -X PUT \
  -H 'Authorization: Bearer replace-me' \
  --data-binary @- \
  http://127.0.0.1:3000/deploy/ghcr.io%2Facme%2Fapp%3Aprod
```

成功响应示例：

```json
{
  "trace_id": "8d460b64-87a7-4cdb-8505-a34fc8b8c869",
  "image_ref": "ghcr.io/acme/app:prod",
  "bytes_received": 12345678,
  "replaced": 1,
  "failed": 0,
  "containers": [
    {
      "container_id": "old-container-id",
      "container_name": "app",
      "new_container_id": "new-container-id",
      "status": "replaced",
      "message": "container replaced successfully"
    }
  ]
}
```

错误响应示例：

```json
{
  "error": "unauthorized"
}
```

常见状态码：

- `200 OK`: 导入镜像并完成容器替换
- `400 Bad Request`: 请求格式错误或镜像名编码错误
- `401 Unauthorized`: 缺少或错误的 Bearer Token
- `404 Not Found`: 镜像不在允许列表中
- `502 Bad Gateway`: engine 调用失败或容器替换失败

## 日志与审计

服务使用 `tracing` 输出 JSON 日志。

每次请求会记录：

- `trace_id`
- 调用来源地址
- 目标镜像名
- 上传大小
- 临时文件路径
- 命中的容器数量
- 每个容器替换结果

注意：

- 日志不会输出 Bearer Token 明文
- 如临时文件删除失败，会输出警告日志

## 当前实现限制

- 仅支持通过 unix socket 调 Docker-compatible HTTP API
- 当前接口只支持导入 tar，不支持 multipart
- 镜像匹配是“完整镜像名 + 标签”精确匹配
- 只替换当前直接使用该镜像名的容器，不理解 compose / stack / app 边界
- 容器重建基于 inspect 结果尽量恢复原配置，但不同 Docker / Podman 版本下，少数高级字段可能存在兼容差异
- 当前没有串行化部署锁；如果同一镜像被并发部署，行为取决于 engine 当前状态

## 示例文件

- 配置示例: [config/deployer.example.toml](/Users/qingyi/Documents/Workspace/PROJ_hachimi/config/deployer.example.toml)

