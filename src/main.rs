use reset_passwd_async::read_conf;
#[tokio::main]
async fn main() {
    // 1.读取主机用户和ip列表
    let users = read_conf("users.conf").unwrap();
    let ips = read_conf("ips.conf").unwrap();

    // 2. 预筛选可用的 IP (异步)
    let check_results = reset_passwd_async::check_ips(ips).await;
    let reachable_ips: Vec<String> = check_results
        .into_iter()
        .filter(|(_, ok)| *ok)
        .map(|(ip, _)| ip)
        .collect();


    // 3. 调用批量检查 (内部使用 JoinSet 实现全并发)
    reset_passwd_async::mass_process(reachable_ips, users).await;
   

}
