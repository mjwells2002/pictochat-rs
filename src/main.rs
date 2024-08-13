#![feature(cursor_remaining)]
#![feature(core_intrinsics)]
#![feature(int_roundings)]
#![feature(extend_one)]
#![feature(ip_bits)]

extern crate core;

use std::{ffi::{c_int, c_void}, mem, net, ptr, sync::mpsc::Sender, thread, time::Duration};
use std::any::Any;
use std::ffi::{c_char, CString};
use std::intrinsics::size_of;
use std::io::ErrorKind::WouldBlock;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{channel, sync_channel};
use std::thread::sleep;
use std::time::Instant;

use byteorder::{ByteOrder, LittleEndian};
use embedded_svc::ws::{FrameType, Receiver};
use esp_idf_hal::{
    gpio::{self, AnyIOPin, InputPin},
    i2c::{I2cConfig, I2cDriver, I2cSlaveConfig, I2cSlaveDriver},
    peripheral::Peripheral,
    peripherals::Peripherals,
    prelude::*,
};
use esp_idf_hal::gpio::{Gpio0, Input, InterruptType, PinDriver};
use esp_idf_hal::spi::{config, Dma, SPI2, SpiDeviceDriver, SpiDriver, SpiDriverConfig};
use esp_idf_svc::{eth, eventloop::{BackgroundLoopConfiguration, EspBackgroundEventLoop,
                                   EspBackgroundSubscription, EspEventLoop, EspSystemEventLoop, User,
}, nvs::EspDefaultNvsPartition, wifi::EspWifi};
use esp_idf_svc::eth::{EspEth, EthDriver, EthEvent, SpiEthChipset};
use esp_idf_svc::eventloop::EspEvent;
use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::netif::{EspNetif, NetifConfiguration};
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::WifiEvent;
use esp_idf_sys::{_g_esp_netif_inherent_eth_config, _g_esp_netif_netstack_default_eth, esp_err_t, esp_eth_config_t, esp_eth_driver_install, esp_eth_handle_t, esp_eth_io_cmd_t_ETH_CMD_S_MAC_ADDR, esp_eth_ioctl, esp_eth_mac_new_w5500, esp_eth_new_netif_glue, ESP_ETH_PHY_ADDR_AUTO, esp_eth_phy_new_w5500, esp_eth_start, ESP_EVENT_ANY_ID, esp_event_base_t, esp_event_handler_instance_register, esp_get_free_heap_size, esp_get_minimum_free_heap_size, esp_ip4_addr_t, esp_netif_attach, esp_netif_config_t, esp_netif_create_default_wifi_ap, esp_netif_destroy, esp_netif_dhcpc_stop, esp_netif_dhcps_stop, esp_netif_get_handle_from_ifkey, esp_netif_init, esp_netif_iodriver_handle, esp_netif_ip_info_t, esp_netif_new, esp_netif_set_ip_info, esp_wifi_beacon_monitor_configure, esp_wifi_config_11b_rate, esp_wifi_config_80211_tx_rate, esp_wifi_get_if_mac, esp_wifi_get_mac, esp_wifi_internal_set_fix_rate, esp_wifi_internal_tx, esp_wifi_power_domain_on, esp_wifi_scan_stop, esp_wifi_set_config, esp_wifi_set_dynamic_cs, esp_wifi_set_mode, esp_wifi_set_protocol, esp_wifi_set_storage, esp_wifi_start, EspError, ETH_EVENT, eth_event_t_ETHERNET_EVENT_CONNECTED, eth_event_t_ETHERNET_EVENT_DISCONNECTED, eth_mac_config_t, eth_phy_config_t, eth_w5500_config_t, gpio_install_isr_service, IP_EVENT, ip_event_t_IP_EVENT_ETH_GOT_IP, ip_event_t_IP_EVENT_ETH_LOST_IP, spi_device_interface_config_t, temperature_sensor_config_t, temperature_sensor_enable, temperature_sensor_get_celsius, temperature_sensor_handle_t, temperature_sensor_install, vTaskDelay, wifi_ap_config_t, wifi_auth_mode_t_WIFI_AUTH_OPEN, wifi_config_t, WIFI_EVENT, wifi_event_ap_staconnected_t, wifi_event_ap_stadisconnected_t, wifi_event_t_WIFI_EVENT_AP_STACONNECTED, wifi_event_t_WIFI_EVENT_AP_STADISCONNECTED, wifi_interface_t_WIFI_IF_AP, wifi_interface_t_WIFI_IF_MAX, wifi_interface_t_WIFI_IF_STA, wifi_mode_t_WIFI_MODE_AP, wifi_mode_t_WIFI_MODE_MAX, wifi_mode_t_WIFI_MODE_NAN, wifi_mode_t_WIFI_MODE_NULL, wifi_mode_t_WIFI_MODE_STA, wifi_netif_driver_t, wifi_phy_mode_t_WIFI_PHY_MODE_11B, wifi_phy_rate_t_WIFI_PHY_RATE_2M_L, wifi_phy_rate_t_WIFI_PHY_RATE_2M_S, wifi_pmf_config_t, WIFI_PROTOCOL_11B, wifi_storage_t_WIFI_STORAGE_RAM};
use esp_idf_sys::esp_wifi_80211_tx;
use ieee80211::MacAddress;
use log::*;
use rmp_serde::{Deserializer, Serializer};
use sctp_proto::PayloadProtocolIdentifier;
use serde::{Deserialize, Serialize};
use utf16string::WString;

use crate::{
    ds_local_wifi_driver::ds_local_wifi_driver::LocalWifiDriver,
    pictochat_app_driver::pictochat_app_driver::PictochatAppDriver,
};
use crate::ds_local_wifi_driver::ds_local_wifi_driver::ClientEvents;
use crate::pictochat_app_driver::pictochat_app_driver::{PictochatEvent, PictochatUserMessageEvent, PictochatUserStateEvent};

mod ds_local_wifi_driver;
mod pictochat_app_driver;
mod raw_wifi;

static BAD_APPLE: &[u8] = include_bytes!("bad_apple.raw");
static mut gbl_tx: Option<Sender<Vec<u8>>> = None;
static mut gbl_tx_nc: Option<Sender<ClientEvents>> = None;
static mut gbl_driver: Option<LocalWifiDriver<PictochatAppDriver>> = None;
static mut gbl_button: Option<PinDriver<Gpio0, Input>> = None;
static mut gbl_mdns: Mutex<Option<EspMdns>> = Mutex::new(None);

static PORT: u16 = 5678;

static mut has_ip: bool = false;

extern "C" {
    fn rom_phy_enable_cca() -> c_void;
    fn rom_phy_disable_cca() -> c_void;

    fn ieee80211_is_tx_allowed() -> bool;
}

fn check_err(err: esp_err_t) {
    //info!("error check {}",err);
    if err != 0 {
        panic!("{}", err);
    }
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub enum PictochatEventNetwork  {
    STATE(PictochatStateEventNetwork),
    MESSAGE(PictochatMessageEventNetwork)
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct PictochatStateEventNetwork {
    mac_address: Vec<u8>,
    is_leaving: bool,
    birthday: u8,
    birthmonth: u8,
    name: String,
    bio: String,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct PictochatMessageEventNetwork {
    mac_address: Option<Vec<u8>>,
    message_content: Vec<u8>
}
unsafe extern "C" fn on_wifievent(event_handler_arg: *mut ::core::ffi::c_void, event_base: esp_event_base_t, event_id: i32, event_data: *mut ::core::ffi::c_void) {
    match event_id as u32 {
        wifi_event_t_WIFI_EVENT_AP_STACONNECTED => {
            unsafe {
                let mut staconnected: wifi_event_ap_staconnected_t = *(event_data as *mut wifi_event_ap_staconnected_t);
                info!("sta connected!, {:?}, {}",staconnected.mac, staconnected.aid);
                let mut tx = gbl_tx_nc.take().unwrap();
                let evt = ClientEvents::NewClient((MacAddress::from_bytes(&staconnected.mac).unwrap(),staconnected.aid));
                let _ = tx.send(evt);
                gbl_tx_nc = Some(tx);
            }
        }
        wifi_event_t_WIFI_EVENT_AP_STADISCONNECTED => {
            unsafe {
                let mut stadisconnected: wifi_event_ap_stadisconnected_t = *(event_data as *mut wifi_event_ap_stadisconnected_t);
                info!("sta disconnected!, {:?}, {}",stadisconnected.mac, stadisconnected.aid);
                let mut tx = gbl_tx_nc.take().unwrap();
                let evt = ClientEvents::LostClient((MacAddress::from_bytes(&stadisconnected.mac).unwrap(),stadisconnected.aid));
                let _ = tx.send(evt);
                gbl_tx_nc = Some(tx);
            }
        }
        _ => {}
    }
}

unsafe fn start_mdns() {
    let mut gmdns = gbl_mdns.lock().unwrap();
    if gmdns.is_none() {
        info!("starting mdns");
        let mut mdns = EspMdns::take().unwrap();
        mdns.set_hostname("pictochat_rs").expect("TODO: panic message");
        mdns.add_service(Some("pictochat_rs powered pictochat node"), "_pictochat", "_tcp", PORT,&[("","")]).expect("TODO: panic message");
        *gmdns = Some(mdns);
    }
}

unsafe fn stop_mdns() {
    let mut gmdns = gbl_mdns.lock().unwrap();
    if gmdns.is_some() {
        let mdns = gmdns.take().unwrap();
        std::mem::drop(mdns);
        info!("stopping mdns");
    }
}

unsafe extern "C" fn on_ethevent(event_handler_arg: *mut ::core::ffi::c_void, event_base: esp_event_base_t, event_id: i32, event_data: *mut ::core::ffi::c_void) {
    match event_id as u32 {
        eth_event_t_ETHERNET_EVENT_CONNECTED => {
            let interface = event_data as *mut esp_eth_handle_t;

            info!("Ethernet Link Up!");
        }
        eth_event_t_ETHERNET_EVENT_DISCONNECTED => {
            let interface = event_data as *mut esp_eth_handle_t;

            stop_mdns();

            info!("Ethernet Link Down!");
        }
        ip_event_t_IP_EVENT_ETH_GOT_IP => {
            info!("Got IP!");

            start_mdns();

            has_ip = true;
        }
        ip_event_t_IP_EVENT_ETH_LOST_IP => {

            stop_mdns();

            info!("Lost IP!");
            has_ip = false;
        }
        a => {
            info!("evt: {}", a);
        }
    }
}

fn main() {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let mut peripherals = Peripherals::take().unwrap();
    let sys_loop = EspSystemEventLoop::take().unwrap();
    let nvs = EspDefaultNvsPartition::take().unwrap();
    let timer_service = EspTaskTimerService::new().unwrap();

    info!("Hello World ...");
    /* listen for events */
    unsafe {
        esp_event_handler_instance_register(WIFI_EVENT, ESP_EVENT_ANY_ID, Some(on_wifievent), ptr::null_mut(), ptr::null_mut());
        esp_event_handler_instance_register(IP_EVENT,ESP_EVENT_ANY_ID,Some(on_ethevent),ptr::null_mut(),ptr::null_mut());
        esp_event_handler_instance_register(ETH_EVENT,ESP_EVENT_ANY_ID,Some(on_ethevent),ptr::null_mut(),ptr::null_mut());
    }

    info!("Initializing WiFi Hardware early to get mac for ethernet");
    let wifi_driver = EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs)).unwrap();
    unsafe { check_err(esp_wifi_set_storage(wifi_storage_t_WIFI_STORAGE_RAM)); }

    let button = PinDriver::input(peripherals.pins.gpio0).unwrap();

    unsafe {gbl_button = Some(button)};

    info!("Setting up Ethernet");

    unsafe {
        let spi_driver_config = SpiDriverConfig::new();

        let spi_driver = SpiDriver::new::<SPI2>(
            peripherals.spi2,
            peripherals.pins.gpio12,
            peripherals.pins.gpio11,
            Some(peripherals.pins.gpio13),
            &spi_driver_config.dma(Dma::Auto(4096)),
        ).unwrap();

        // the safe api above will deinitalize the spi peripheral when it is dropped,
        // as we call into the C apis for ethernet, need to make sure it won't be dropped

        let spi_driver_leak = Box::leak(Box::new(spi_driver));

        // i tried to use the safe api wrapper for setting up ethernet,
        // but it didn't work, and the mcu crashed each time i used it, so unsafe api it is

        let mac_config = eth_mac_config_t {
            sw_reset_timeout_ms: 100,
            rx_task_stack_size: 4096,
            rx_task_prio: 15,
            flags: 0,
        };

        let phy_config = eth_phy_config_t {
            phy_addr: ESP_ETH_PHY_ADDR_AUTO,
            reset_timeout_ms: 100,
            autonego_timeout_ms: 4000,
            reset_gpio_num: 4,
        };

        let mut spi_config = spi_device_interface_config_t {
            mode: 0,
            clock_speed_hz: 40 * 1000 * 1000,
            spics_io_num: 15,
            queue_size: 20,

            command_bits: 0,
            address_bits: 0,
            dummy_bits: 0,
            clock_source: 0,
            duty_cycle_pos: 0,
            cs_ena_pretrans: 0,
            cs_ena_posttrans: 0,
            input_delay_ns: 0,
            flags: 0,
            pre_cb: None,
            post_cb: None,
        };

        let w5500_config = eth_w5500_config_t {
            spi_host_id: 1,
            spi_devcfg: &mut spi_config,
            int_gpio_num: 5,
            poll_period_ms: 0,
            custom_spi_driver: Default::default(),
        };

        gpio_install_isr_service(0);

        let eth_mac = esp_eth_mac_new_w5500(&w5500_config, &mac_config);
        let eth_phy = esp_eth_phy_new_w5500(&phy_config);

        let eth_config = esp_eth_config_t {
            mac: eth_mac,
            phy: eth_phy,
            check_link_period_ms: 2000,
            stack_input: None,
            on_lowlevel_init_done: None,
            on_lowlevel_deinit_done: None,
            read_phy_reg: None,
            write_phy_reg: None,
        };

        let mut eth_handle  = Box::into_raw(Box::new(*(ptr::null_mut() as *mut esp_eth_handle_t)));

        esp_eth_driver_install(&eth_config, eth_handle as *mut esp_eth_handle_t);

        let mut eth_mac_addr: [u8; 6] = [0u8,0u8,0u8,0u8,0u8,0u8];

        esp_wifi_get_mac(wifi_interface_t_WIFI_IF_STA,eth_mac_addr.as_mut_ptr());

        esp_eth_ioctl(*eth_handle, esp_eth_io_cmd_t_ETH_CMD_S_MAC_ADDR, eth_mac_addr.as_mut_ptr() as *mut c_void);

        esp_netif_init();

        let netif_cfg = esp_netif_config_t {
            base: &_g_esp_netif_inherent_eth_config,
            driver: ptr::null(),
            stack: _g_esp_netif_netstack_default_eth,
        };

        let eth_netif = esp_netif_new(&netif_cfg);

        esp_netif_attach(eth_netif, esp_eth_new_netif_glue(*eth_handle) as esp_netif_iodriver_handle);

        esp_eth_start(*eth_handle);
    }

    info!("Ethernet hardware setup complete!");
    info!("Waiting upto 10 seconds for Ethernet IP");
    let now = Instant::now();
    unsafe {
        while !has_ip && now.elapsed() < Duration::from_secs(10)  {
            sleep(Duration::from_millis(100));
        }
        if has_ip {
            info!("Got IP!");
        } else {
            warn!("Failed to get ip, continuing anyway");
        }
    }


    info!("Setting Up WiFi");


    /* leak the wifi driver so the safe api doesn't try to deintialize the hardware */
    let wifi_driver_leak = Box::leak(Box::new(wifi_driver));

    let mut mac: [u8; 6] = [0u8,0u8,0u8,0u8,0u8,0u8];

    unsafe {
        let mut ssid = [0u8;32];
        ssid[4] = 01u8;
        //let testssid = "Hello World";
        //ssid[..testssid.len()].copy_from_slice(testssid.as_bytes());
        let mut wifi_config = wifi_config_t {
            ap: wifi_ap_config_t {
                ssid: ssid,
                ssid_len: 32,
                channel: 7,
                authmode: wifi_auth_mode_t_WIFI_AUTH_OPEN,
                pmf_cfg: wifi_pmf_config_t {
                    capable: false,
                    required: false,
                },
                beacon_interval: 100,
                max_connection: 16,
                ssid_hidden: 1,
                ..Default::default()
            }
        };


        check_err(esp_wifi_set_mode(wifi_mode_t_WIFI_MODE_AP));
        check_err(esp_wifi_set_config(wifi_interface_t_WIFI_IF_AP,&mut wifi_config));

        check_err(esp_wifi_set_protocol(wifi_interface_t_WIFI_IF_AP, wifi_phy_mode_t_WIFI_PHY_MODE_11B as u8));
        check_err(esp_wifi_config_80211_tx_rate(wifi_interface_t_WIFI_IF_AP,wifi_phy_rate_t_WIFI_PHY_RATE_2M_S));
        check_err(esp_wifi_start());
        check_err(esp_wifi_internal_set_fix_rate(wifi_interface_t_WIFI_IF_AP,true,wifi_phy_rate_t_WIFI_PHY_RATE_2M_S));

        check_err(esp_wifi_get_mac(wifi_interface_t_WIFI_IF_AP,mac.as_mut_ptr()));
        check_err(esp_wifi_set_protocol(wifi_interface_t_WIFI_IF_AP,WIFI_PROTOCOL_11B as u8));
        info!("Started SoftAP");

        let wifi_ap = esp_netif_get_handle_from_ifkey(c"WIFI_AP_DEF".as_ptr());
        let res = esp_netif_dhcps_stop(wifi_ap);
        info!("Stopping default DHCP Server result={}",res);

        info!("WiFi MAC address is: {:?}", mac);
    }

    info!("WiFi Setup Complete");

    info!("Setting up Application");

    let (pictochat_from_network_tx, pictochat_from_network_rx) = sync_channel::<PictochatEvent>(10);
    let (pictochat_to_network_tx, pictochat_to_network_rx) = sync_channel::<PictochatEvent>(10);

    let mut pictochat_from_network_rx_ma = Arc::new(Mutex::new(pictochat_from_network_rx));
    let mut pictochat_to_network_rx_ma = Arc::new(Mutex::new(pictochat_to_network_rx));

    let mut driver = LocalWifiDriver::new(
        PictochatAppDriver {
            i: 0,
            seq_no: 0,
            next_pkt: None,
            outgoing_events: Some(pictochat_to_network_tx),
            incoming_events: Some(Arc::clone(&pictochat_from_network_rx_ma)),
            ..Default::default()
        });

    let (rx, nctx) = driver.start(mac);

    unsafe {
        gbl_tx_nc = Some(nctx);
        gbl_driver = Some(driver);
    }

    info!("Ready!");

    let mut config = Configuration::default();
    config.http_port = PORT;
    let mut server = EspHttpServer::new(&config).unwrap();

    server.ws_handler("/ws",move |ws| {
        if ws.is_new() {
            info!("Got a new connection!");
            let mut detached_sender = ws.create_detached_sender().expect("TODO: panic message");
            let arc_clone = Arc::clone(&pictochat_to_network_rx_ma);
            thread::Builder::new().stack_size(8 * 1024).spawn(move || {
                let chan = arc_clone.lock().expect("TODO: panic message");
                loop {
                    if let Ok(data) = chan.recv_timeout(Duration::from_millis(1000)) {
                        let netevent = match data {
                            PictochatEvent::STATE(state) => {
                                PictochatEventNetwork::STATE(PictochatStateEventNetwork {
                                    mac_address: state.mac_address.as_bytes().to_vec(),
                                    is_leaving: state.is_leaving,
                                    birthday: state.birthday,
                                    birthmonth: state.birthmonth,
                                    name: state.name.to_utf8(),
                                    bio: state.bio.to_utf8(),
                                })
                            }
                            PictochatEvent::MESSAGE(message) => {
                                PictochatEventNetwork::MESSAGE(PictochatMessageEventNetwork {
                                    mac_address: Some(message.mac_address.unwrap().as_bytes().to_vec()),
                                    message_content: message.message_content,
                                })
                            }
                        };
                        let mut msgpack_data = Vec::new();
                        let mut serializer = rmp_serde::Serializer::new(&mut msgpack_data)
                            .with_bytes(rmp_serde::config::BytesMode::ForceAll);
                        netevent.serialize(&mut serializer).unwrap();
                        detached_sender.send(FrameType::Binary(false), &msgpack_data).expect("TODO: panic message");
                    }
                    if detached_sender.is_closed() {
                        break;
                    }
                }
            }).expect("TODO: panic message");
            return Ok(());
        }

        if ws.is_closed() {
            info!("Connection closed");
            return Ok(());
        }

        let (frame_type, len) = match ws.recv(&mut []) {
            Ok(frame) => frame,
            Err(e) => return Err(e),
        };
        match frame_type {
            FrameType::Text(_) => {
                info!("text");
            }
            FrameType::Binary(frag) => {
                info!("binary fragmented {}",frag);
            }
            FrameType::Ping => {}
            FrameType::Pong => {}
            FrameType::Close => {}
            FrameType::SocketClose => {}
            FrameType::Continue(_) => {}
        }
        if len <= 10240*2 {
            let mut buf = vec![0u8; len];
            ws.recv(&mut buf).expect("TODO: panic message");
            info!("got packet len: {}",len);

            let event = rmp_serde::from_slice::<PictochatEventNetwork>(&buf).unwrap();

            let intevent = match event {
                PictochatEventNetwork::STATE(state) => {
                    panic!("unsupported network message inbound")
                }
                PictochatEventNetwork::MESSAGE(message) => {
                    PictochatEvent::MESSAGE(PictochatUserMessageEvent {
                        mac_address: None,
                        message_content: message.message_content,
                    })
                }
            };

                pictochat_from_network_tx.send(intevent).expect("TODO: panic message");
        } else {
            warn!("msg too big; ignoring");
        }

        Ok(())
    }).expect("TODO: panic message");

    let (temp_handle,temp_read_buf) = unsafe {
        let temp_handle = Box::into_raw(Box::new(*(null_mut() as *mut temperature_sensor_handle_t)));
        let temp_sensor = temperature_sensor_config_t {
            range_min: -10,
            range_max: 80,
            clk_src: 0,
        };
        temperature_sensor_install(&temp_sensor, temp_handle);
        temperature_sensor_enable(*temp_handle);

        let temp_read_buf = Box::into_raw(Box::new(0f32));
        temperature_sensor_get_celsius(*temp_handle,temp_read_buf);
        info!("cpu temp {}", *temp_read_buf);
        (temp_handle,temp_read_buf)
    };


    loop {
        unsafe {
            let min_free = esp_get_minimum_free_heap_size();
            let free = esp_get_free_heap_size();

            temperature_sensor_get_celsius(*temp_handle,temp_read_buf);

            info!("Free Ram: {}/{}",min_free,free);
            //vTaskDelay(1000);
        }



        match rx.recv_timeout(Duration::from_millis(1000)) {
            Ok(data) => unsafe {

            },
            Err(_) => {}
        }
    }
}

fn log_packet(log: &[u8]) {
    unsafe {
        //let now = Instant::now();
        let mut tx = gbl_tx.take().unwrap();
        //let mut driver = gbl_driver.take().unwrap();
        let mut npkt = Vec::new();
        npkt.extend_from_slice(log);
        let _ = tx.send(npkt);
        gbl_tx = Some(tx);
        //gbl_driver = Some(driver);
        //info!("time to send to channel, {}",now.elapsed().as_micros());

    }
}

#[no_mangle]
pub unsafe extern "C" fn ieee80211_raw_frame_sanity_check(_arg: i32, _arg2: i32, _arg3: i32) -> i32 {
    //info!("sanity check len {}",_arg3);
    return 0;
}
//this function normally adds the dsparams tag and returns ptr+sizeof(dsparms)
// we are going to hook it to go backwards 8 bytes, remove the ssid tag,
//  rewrite the already written supported rates tag and write in a new dsparams tag

#[no_mangle]
pub unsafe extern "C" fn ieee80211_add_dsparams(data: *mut u8) -> *mut u8 {
    info!("ieee80211_add_dsparams hook");
    let start_ssid = data.offset(-8);
    let start_rates = data.offset(-6);
    let mut rates = [0u8;6];
    start_rates.copy_to(rates.as_mut_ptr(),6);
    start_ssid.copy_from(rates.as_ptr(),6);
    let start_dsparams = data.offset(-2);
    //TODO: dont hardcode wifi channel 7
    let dsparams = [0x03u8,0x01,0x07];
    start_dsparams.copy_from(dsparams.as_ptr(),3);
    return data.offset(1);
}