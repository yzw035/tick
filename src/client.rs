use std::{env, time::Duration};

use crate::{
    clients::dm::DmClient,
    errors::ClientError,
    models::{
        perform::{PerformForm, PerformInfo, PerformItem, PerformParams, SkuItem},
        task::Task,
        ticket::{
            GetTicketListForm, GetTicketListParams, Ticket, TicketInfo, TicketInfoForm,
            TicketInfoParams, TicketList,
        },
    },
    ticket::DmTicket,
};
use anyhow::Result;
use chrono::{Local, TimeZone};
use fast_qr::{QRBuilder, QRCode};
use log::{debug, info};
use terminal_menu::{button, label, menu, mut_menu, numeric, run};
use thirtyfour::{prelude::ElementQueryable, By, DesiredCapabilities, WebDriver};
use tokio::{fs, io::AsyncWriteExt};

pub struct Client {
    webdriver_url: String,
}

impl Client {
    pub fn new(webdriver_url: String) -> Self {
        Self { webdriver_url }
    }

    pub async fn get_driver(&self, webdriver_url: String) -> Result<WebDriver> {
        let mut caps = DesiredCapabilities::chrome();
        caps.set_disable_dev_shm_usage()?;
        caps.set_headless()?;
        caps.set_disable_gpu()?;
        caps.set_disable_web_security()?;
        caps.set_ignore_certificate_errors()?;
        caps.add_chrome_arg("--disable-blink-features=AutomationControlled")?;
        caps.add_chrome_arg("--disable-logging")?;
        //caps.add_chrome_arg("--blink-settings=imagesEnabled=false")?;
        caps.add_chrome_arg("--incognito")?;
        caps.add_chrome_arg("--disable-stylesheet")?;
        caps.add_chrome_arg("--excludeSwitches=[\"enable-automation\"]")?;
        caps.add_chrome_arg("--useAutomationExtension=false")?;
        caps.add_chrome_arg("--disable-infobars")?;
        caps.add_chrome_arg("--disable-software-rasterizer")?;
        caps.add_chrome_arg("--disable-extensions")?;
        caps.add_chrome_arg("--no-sandbox")?;
        caps.add_chrome_arg("--user-agent=Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36")?;
        caps.add_chrome_arg("--window-size=1920,1080")?;
        caps.add_chrome_arg("--single-process")?;
        let driver: WebDriver = WebDriver::new(&webdriver_url, caps.clone())
            .await
            .map_err(|_| ClientError::WebdriverConnectionError)?;
        Ok(driver)
    }

    pub async fn get_qrcode(&self, url: &str) -> Result<QRCode> {
        let qrcode_path = env::var("QRCODE_PATH").unwrap();

        let client = reqwest::Client::builder().build()?;

        let mut source = client.get(url).send().await?;

        let mut dest = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&qrcode_path)
            .await?;

        while let Some(chunk) = source.chunk().await? {
            dest.write_all(&chunk).await?;
        }

        let img = image::open(&qrcode_path)?.to_luma8();

        let mut img = rqrr::PreparedImage::prepare(img);

        let grids = img.detect_grids();
        let (_, content) = grids[0].decode()?;

        let qrcode = QRBuilder::new(content).build().unwrap();

        let _ = fs::remove_file(qrcode_path).await;

        Ok(qrcode)
    }

    pub async fn login(&self) -> Result<(String, String)> {
        info!("正在获取登录二维码...");

        debug!("正在打开浏览器...");
        let driver = self.get_driver(self.webdriver_url.clone()).await?;

        // 进入登录页面
        let login_url = "https://passport.damai.cn/login?ru=https%3A%2F%2Fwww.damai.cn%2F";
        debug!("正在打开登录页面, url: {}", login_url);
        driver
            .goto(login_url)
            .await
            .map_err(|_| ClientError::GetQRCodeError)?;

        // 等待出现登录框
        debug!("查找登录iframe!");
        let iframe_name = "alibaba-login-iframe";
        let _ = driver
            .query(By::ClassName(iframe_name))
            .wait(Duration::from_secs(10), Duration::from_millis(100))
            .first()
            .await;
        let _ = driver.enter_frame(0).await;

        // 选择扫码登录
        debug!("点击扫码登录!");
        let element_xpath = r#"//*[@id="login-tabs"]/div[3]"#;
        let element = driver.query(By::XPath(element_xpath)).first().await?;
        element.click().await?;

        // 获取二维码
        debug!("获取二维码!");
        let element_selector = r#"#login > div.login-content.nc-outer-box > div > div:nth-child(2) > div.qrcode-img > img"#;
        let element = driver.query(By::Css(element_selector)).first().await?;
        let src = element
            .attr("src")
            .await
            .map_err(|_| ClientError::GetQRCodeError)?;
        if src.is_none() {
            let _ = driver.quit().await;
            return Ok(("".to_string(), "".to_string()));
        }
        let url = src.unwrap();

        let qrcode = self.get_qrcode(&url).await?;

        qrcode.print();

        info!("请打开大麦APP扫码登录...");

        let css = r#"body > div.dm-header-wrap > div > div.right-header > div.box-header.user-header > a.J_userinfo_name > div"#;

        let mut nickname = String::new();
        let mut login_success = false;
        for _ in 0..60 {
            let res = driver
                .query(By::Css(css))
                .wait(Duration::from_secs(5), Duration::from_millis(100))
                .first()
                .await;
            if res.is_err() {
                continue;
            }
            let element = res.unwrap();
            let text = element.text().await;
            if text.is_err() {
                continue;
            }
            nickname = text.unwrap();
            login_success = true;
            break;
        }

        if !login_success {
            info!("扫码登录未成功, 退出...");
            driver.quit().await?;
            return Ok(("".to_string(), "".to_string()));
        }

        info!("用户昵称:{}, 登录成功...", nickname);

        debug!("跳到h5用户信息页面!");
        let h5_url = "https://m.damai.cn/damai/mine/my/index.html?spm=a2o71.home.top.duserinfo";
        driver.goto(h5_url).await?;

        debug!("等待页面加载完成, 获取cookie!");
        let css = r#"body > div.my > div.my-hd > div.user-name > div.nickname"#;
        let _ = driver
            .query(By::Css(css))
            .wait(Duration::from_secs(10), Duration::from_millis(100))
            .first()
            .await;
        let cookies = driver.get_all_cookies().await?;

        let mut cookie_string = String::new();

        for item in cookies {
            if item.name().starts_with("_m_h5_tk") {
                continue;
            }
            cookie_string.push_str(&format!("{}={};", item.name(), item.value()));
        }

        let _ = driver.quit().await;

        Ok((cookie_string, nickname))
    }

    // 获取演唱会ID
    pub async fn get_ticket_id(&self) -> Result<Ticket> {
        let dm = DmClient::new(None, None).await?;
        let url = "https://mtop.damai.cn/h5/mtop.damai.wireless.search.broadcast.list/1.0/";
        let params = GetTicketListParams::build()?;
        let form = GetTicketListForm::build()?;

        let res = dm.request(url, params, form).await?;

        // 今日必抢
        let today_ticket_list: TicketList = serde_json::from_value(res.data["modules"][0].clone())?;

        // 即将开抢
        let ticket_list: TicketList = serde_json::from_value(res.data["modules"][1].clone())?;

        let mut tickets: Vec<Ticket> = Vec::new();

        for ticket in today_ticket_list.items {
            if !ticket.category_name.contains("演唱会") {
                continue;
            }
            tickets.push(ticket);
        }

        for ticket in ticket_list.items {
            if !ticket.category_name.contains("演唱会") {
                continue;
            }
            tickets.push(ticket);
        }

        let mut select_list = vec![label("请选择演唱会:")];
        for ticket in tickets.iter() {
            let date_time = Local.timestamp_millis_opt(ticket.sale_time as i64).unwrap();
            select_list.push(button(format!(
                "{}, 开抢时间:{}",
                ticket.ticket_name,
                date_time.format("%Y-%m-%d %H:%M:%S")
            )));
        }
        let m = menu(select_list);
        run(&m);
        let index = mut_menu(&m).selected_item_index() - 1;

        Ok(tickets[index].clone())
    }

    pub async fn get_perform(&self, ticket_id: &String) -> Result<PerformItem> {
        let dm = DmClient::new(None, None).await?;

        let url = "https://mtop.damai.cn/h5/mtop.alibaba.damai.detail.getdetail/1.2";

        let params = TicketInfoParams::build()?;

        let data = TicketInfoForm::build(ticket_id)?;

        let res = dm.request(url, params, data).await?;

        let ticket_info: TicketInfo =
            serde_json::from_str(res.data["result"].clone().as_str().unwrap())?;

        let perform_list = ticket_info
            .detail_view_component_map
            .item
            .item
            .perform_bases;

        let mut performs: Vec<PerformItem> = Vec::new();

        for perform in perform_list.iter() {
            for item in perform.performs.iter() {
                performs.push(PerformItem {
                    perfrom_name: item.perform_name.clone(),
                    perform_id: item.perform_id.clone(),
                })
            }
        }

        let mut select_list = vec![label("请选择场次:")];

        for perform in performs.iter() {
            select_list.push(button(perform.perfrom_name.clone()));
        }

        let m = menu(select_list);
        run(&m);

        let index = mut_menu(&m).selected_item_index() - 1;

        Ok(performs[index].clone())
    }

    pub async fn get_sku(&self, ticket_id: String, perfrom_id: String) -> Result<SkuItem> {
        let dm = DmClient::new(None, None).await?;

        let url = "https://mtop.damai.cn/h5/mtop.alibaba.detail.subpage.getdetail/2.0/";

        let params = PerformParams::build()?;

        let data = PerformForm::build(&ticket_id, &perfrom_id)?;

        let res = dm.request(url, params, data).await?;

        let perform_info: PerformInfo = serde_json::from_str(res.data["result"].as_str().unwrap())?;

        let mut skus: Vec<SkuItem> = vec![];
        for item in perform_info.perform.sku_list.iter() {
            skus.push(SkuItem {
                sku_id: item.sku_id.clone(),
                sku_name: item.price_name.clone(),
            })
        }

        let mut select_list = vec![label("请选择票档:")];
        for sku in skus.iter() {
            select_list.push(button(sku.sku_name.clone()));
        }

        let m = menu(select_list);
        run(&m);
        let index = mut_menu(&m).selected_item_index() - 1;

        Ok(skus[index].clone())
    }

    pub async fn run(&self) -> Result<()> {
        let (cookie, nickname) = self.login().await.map_err(|_| ClientError::LoginFailed)?;

        let ticket = self.get_ticket_id().await?;

        let perform = self.get_perform(&ticket.ticket_id.to_string()).await?;

        let sku = self
            .get_sku(ticket.ticket_id.to_string(), perform.perform_id.to_string())
            .await?;

        let m = menu(vec![
            numeric(
                "购票数量",
                1.0, //default
                Some(1.0),
                Some(1.0),
                Some(4.0),
            ),
            button("确定"),
        ]);
        run(&m);
        let ticket_num = mut_menu(&m).numeric_value("购票数量");

        let m = menu(vec![
            numeric("重试次数", 5.0, Some(1.0), None, Some(10.0)),
            button("确定"),
        ]);
        run(&m);
        let retry_times = mut_menu(&m).numeric_value("重试次数");

        let m = menu(vec![
            numeric("重试间隔", 100.0, Some(10.0), None, Some(1000.0)),
            button("确定"),
        ]);
        run(&m);
        let retry_interval = mut_menu(&m).numeric_value("重试间隔");

        let m = menu(vec![
            numeric("生成-提交订单间隔(毫秒)", 30.0, Some(10.0), Some(0.0), None),
            button("确定"),
        ]);
        run(&m);
        let wati_for_submit_interval = mut_menu(&m).numeric_value("生成-提交订单间隔(毫秒)");

        let m = menu(vec![
            numeric(
                "请求时间偏移量(毫秒)",
                0.0,
                Some(10.0),
                Some(-100.0),
                Some(1000.0),
            ),
            button("确定"),
        ]);
        run(&m);
        let request_time_offset = mut_menu(&m).numeric_value("请求时间偏移量(毫秒)");

        let m = menu(vec![
            numeric("优先购时长(分钟)", 0.0, Some(20.0), Some(0.0), Some(60.0)),
            button("确定"),
        ]);
        run(&m);
        let priority_purchase_time = mut_menu(&m).numeric_value("优先购时长(分钟)");

        let task = Task {
            nickname,
            ticket_id: ticket.ticket_id.to_string(),
            ticket_name: ticket.ticket_name.to_string(),
            ticket_perform_id: perform.perform_id.to_string(),
            ticket_perform_name: perform.perfrom_name,
            ticket_perform_sku_id: sku.sku_id,
            ticket_perform_sku_name: sku.sku_name,
            ticket_num: ticket_num as usize,
            priority_purchase_time: priority_purchase_time as i64,
            request_time_offset: request_time_offset as i64,
            retry_interval: retry_interval as u64,
            retry_times: retry_times as u64,
            wait_for_submit_interval: wati_for_submit_interval as u64,
            real_names: vec![],
        };

        let mut app = DmTicket::new(cookie, task).await?;
        app.run().await?;

        Ok(())
    }
}
