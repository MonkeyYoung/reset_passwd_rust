use futures::future::join_all;
use std::process::Stdio;
use std::time::Duration;
use std::{
    fs::File,
    io::{self, BufRead, BufReader},
    net::SocketAddr,
};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::task::JoinSet;
use tokio::time::timeout;
use std::sync::Arc;
use tokio::sync::{Semaphore, Mutex};
use rand::rngs::OsRng;
use rand::prelude::SliceRandom;
use chrono::Local;
use std::collections::{BTreeMap, BTreeSet};
use umya_spreadsheet::{new_file, writer::xlsx::write};

// 新密码结构体
pub struct PasswdTask {
    pub ip: String,
    pub user: String,
    pub new_pass: String,
}
// 读取文件获取内容
pub fn read_conf(filename: &str) -> io::Result<Vec<String>> {
    let file = File::open(filename)?;
    let reader = BufReader::new(file);

    let mut users_or_ips = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let user = line.trim();
        if !user.is_empty() {
            users_or_ips.push(user.to_string());
        }
    }

    Ok(users_or_ips)
}
// ip连通性测试，异步。

async fn check_ip(ip: String) -> (String, bool) {
    let addr = format!("{}:22", ip);
    let ok = if let Ok(socket_addr) = addr.parse::<SocketAddr>() {
        // 1. timeout 会返回 Result<Result<...>, Elapsed>
        // 2. .await 得到结果
        // 3. .ok() 将其转为 Option<Result<TcpStream, io::Error>>，超时则为 None
        // 4. .and_then(|res| res.ok()) 将内层 Result 转为 Option
        // 5. .is_some() 最终判断是否成功连接
        timeout(Duration::from_secs(2), TcpStream::connect(socket_addr))
            .await
            .ok()
            .and_then(|res| res.ok())
            .is_some()
    } else {
        false
    };

    println!("{} [CHECK] 🔍 {} is {}", Local::now().format("%Y-%m-%d %H:%M:%S"), ip, if ok { "reachable ✅" } else { "unreachable ❌" });
    (ip, ok)
}
pub async fn check_ips(ips: Vec<String>) -> Vec<(String, bool)> {

    // 打印开始检查的日志
    println!("---------------------------------------------------------------------------");
    println!("{} [INFO] 🚀 开始检查 IP 连通性: 共 {} 个 IP", Local::now().format("%Y-%m-%d %H:%M:%S"), ips.len());
    println!("---------------------------------------------------------------------------");

    let futures = ips.into_iter().map(check_ip);
    join_all(futures).await
}

// 测试用户合法性
pub async fn check_user_on_ip_async(user: &str, ip: &str) -> bool {
    // 构造远程执行的命令：id -u {user}
    // -u 只返回 UID，比单纯的 id 更轻量
    
    let remote_cmd = format!("id -u {}", user);

    let mut child = Command::new("ssh");
    child
        .args([
            "-o",
            "BatchMode=yes", // 禁止交互式输入（密码等）
            "-o",
            "ConnectTimeout=5", // 连接超时
            "-o",
            "StrictHostKeyChecking=no", // 自动接受主机密钥
            "-o",
            "PasswordAuthentication=no", // 强制只使用公钥/免密验证
            ip,
            &remote_cmd,
        ])
        .stdout(Stdio::null()) // 丢弃标准输出
        .stderr(Stdio::null()); // 丢弃错误输出

    // 为整个进程执行设置一个硬超时（防止进程僵死）
    match timeout(Duration::from_secs(8), child.status()).await {
        Ok(Ok(status)) => status.success(),
        _ => false, // 超时或执行出错均返回 false
    }
}

pub async fn mass_process(ips: Vec<String>, users: Vec<String>) {
    let total_ips = ips.len();
    let total_users = users.len();
    let total_tasks = total_ips * total_users;
    
    let mut set = JoinSet::new();
    let sem = Arc::new(Semaphore::new(50));
    
    // 计数器，用于最后统计
    let success_count = Arc::new(Mutex::new(0));
    let fail_count = Arc::new(Mutex::new(0));

    println!("---------------------------------------------------------------------------");
    println!("{} [INFO] 🚀 开始处理任务: {} 台主机, {} 个待修改用户, 共 {} 个tasks", 
             Local::now().format("%Y-%m-%d %H:%M:%S"), total_ips, total_users, total_tasks);
    println!("---------------------------------------------------------------------------");

    for ip in ips {
        for user in &users {
            let u = user.clone();
            let i = ip.clone();
            let permit = Arc::clone(&sem);
            let s_acc = Arc::clone(&success_count);
            let f_acc = Arc::clone(&fail_count);

            set.spawn(async move {
                let _p = permit.acquire_owned().await.unwrap();


                
                // 1. 检查用户是否存在
                if check_user_on_ip_async(&u, &i).await {
                    // 2. 生成新密码
                    let new_pass = generate_strong_password(12);
                    //let new_pass = "czrP6T9YiPux".to_string(); // 统一密码，便于后续登录
                    // 3. 执行修改逻辑
                    if change_password_async(&u, &i, &new_pass).await {
                        let mut count = s_acc.lock().await;
                        *count += 1;
                         Some((i, u, new_pass, true))
                    } else {
                        let mut count = f_acc.lock().await;
                        *count += 1;
                         Some((i, u, "PASSWORD_CHANGE_FAILED".to_string(), false))
                    }
                }else {
                    let mut count = f_acc.lock().await;
                    *count += 1;
                     Some((i, u, "USER_NOT_FOUND".to_string(), false))               
                }

            });
        }
    }

    // 保存结果到Excel
    let mut tasks: Vec<PasswdTask> = Vec::new();
    // 4. 实时收集结果并打印
    while let Some(res) = set.join_next().await {
        let now = Local::now().format("%Y-%m-%d %H:%M:%S");
        match res {
            Ok(Some((ip, user, pass, true))) => {
                // 成功修改密码
                // println!("{} [SUCCESS] ✅ 用户 {:<10} @ {:<15} | 新密码: {}", now, user, ip, pass);
                println!("{} [SUCCESS] ✅ 用户 {:<10} @ {:<15} | 新密码: ********** ", now, user, ip);
                // 创建password task结构体
                let task = PasswdTask {
                    ip: ip.clone(),
                    user: user.clone(),
                    new_pass: pass.clone(),
                };
                tasks.push(task);
                
            }
            Ok(Some((ip, user, err_msg, false))) => {
                println!("{} [ERROR]   ❌ 用户 {:<10} @ {:<15} | 原因: {}", now, user, ip, err_msg);
            }
            Ok(None) => {
                // 不会发生
            } 
            Err(e) => println!("{} [CRITICAL] 💥 任务执行异常: {:?}", now, e),
        }
    }
    // 保存到Excel
    if let Err(e) = save_pass_to_excel(&tasks, "pd.xlsx") {
        eprintln!("{} [ERROR] 💾 保存到Excel失败: {}", Local::now().format("%Y-%m-%d %H:%M:%S"), e);
    } else {
        println!("{} [INFO] 💾 密码已保存到 pd.xlsx", Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
    println!("---------------------------------------------------------------------------");
    println!("{} [FINISH] 🏁 处理完毕! 成功: {} | 失败: {}", 
             Local::now().format("%Y-%m-%d %H:%M:%S"), 
             *success_count.lock().await, 
             *fail_count.lock().await);
    println!("---------------------------------------------------------------------------");
}

// 修改密码的核心执行逻辑
pub async fn change_password_async(user: &str, ip: &str, new_pass: &str) -> bool {
    // 使用 chpasswd 这种非交互式方式：echo "user:pass" | chpasswd
    let remote_cmd = format!("echo '{}:{}' | sudo chpasswd", user, new_pass);

    let mut child = Command::new("ssh");
    child.args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=no",
            ip,
            &remote_cmd,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    match timeout(Duration::from_secs(10), child.status()).await {
        Ok(Ok(status)) => status.success(),
        _ => false,
    }
}

// 密码生成
const LOWER: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const UPPER: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const DIGIT: &[u8] = b"0123456789";
const SYMBOL: &[u8] = b",.:;!@#$%^&*()-_=+[]{}";

pub fn generate_strong_password(len: usize) -> String {
    assert!(len >= 8, "password length too short");

    let mut rng = OsRng;

    let mut chars = Vec::with_capacity(len);

    // 强制每一类至少一个
    chars.push(*LOWER.choose(&mut rng).unwrap());
    chars.push(*UPPER.choose(&mut rng).unwrap());
    chars.push(*DIGIT.choose(&mut rng).unwrap());
    chars.push(*SYMBOL.choose(&mut rng).unwrap());

    // 剩余随机
    let all: Vec<u8> = [LOWER, UPPER, DIGIT, SYMBOL].concat();
    for _ in chars.len()..len {
        chars.push(*all.choose(&mut rng).unwrap());
    }

    // 打乱顺序，避免规则痕迹
    chars.shuffle(&mut rng);

    String::from_utf8(chars).unwrap()
}
// 保存结果到excel
pub fn save_pass_to_excel(tasks: &[PasswdTask], path: &str)  -> Result<(), Box<dyn std::error::Error>> {

    // 创建新的工作表
    let mut book = new_file();
    let sheet = book.get_sheet_by_name_mut("Sheet1").unwrap();

    // 创建一个BTreeMap用于存放以IP为key的Map集合
    // sorted_users 用户用户名排序
    let mut all_users:  BTreeSet<&str> = BTreeSet::new();
    
    for task in tasks {
        all_users.insert(&task.user);
    }

    let sorted_users: Vec<&str> = all_users.into_iter().collect();
    let mut matrix: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();

    for task in tasks {
        matrix.entry(&task.ip).or_default().insert(&task.user, &task.new_pass);
    } 

    // 核心逻辑，遍历matrix集合，存入excel
    
        let mut row =1;  
    for (ip,user_map) in matrix {

        sheet.get_cell_mut((1,row)).set_value(ip);
        let mut col = 2;
        for user_name in &sorted_users {
            
            if let Some(pass) = user_map.get(user_name) {
                sheet.get_cell_mut((col,row)).set_value(*user_name);
                col +=1;
                sheet.get_cell_mut((col,row)).set_value(*pass);
                col +=1;
            }else {
                sheet.get_cell_mut((col,row)).set_value("-");
                col +=1;
                sheet.get_cell_mut((col,row)).set_value("-");
                col +=1;
            }
        }
        row +=1;

    }

    write(&book, path)?;
    Ok(())

}