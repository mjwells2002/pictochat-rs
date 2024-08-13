use esp_idf_sys::{
    self as _, esp_wifi_set_channel, esp_wifi_set_mode, esp_wifi_set_promiscuous,
    esp_wifi_set_promiscuous_rx_cb, esp_wifi_set_ps, sigsuspend, tskTaskControlBlock, vTaskSuspend,
    wifi_pkt_rx_ctrl_t, wifi_promiscuous_pkt_t, wifi_ps_type_t_WIFI_PS_NONE,
    wifi_second_chan_t_WIFI_SECOND_CHAN_NONE, TaskHandle_t,
};
use log::info;

pub static mut INSTANCE: RawWifiInstanceHolder = RawWifiInstanceHolder { instance: None };
pub struct RawWifiInstanceHolder {
    instance: Option<RawWifi>,
}

impl RawWifiInstanceHolder {
    pub fn init(&mut self) {
        self.instance = Some(RawWifi::new(1));
    }
    pub fn get(&mut self) -> &mut RawWifi {
        match &mut (self.instance) {
            Some(rinst) => rinst,
            None => panic!("No"),
        }
    }
}

pub struct RawWifi {
    channel: u8,
    log_fn: Option<fn(&[u8])>,
}

impl RawWifi {
    fn new(wifi_channel: u8) -> Self {
        RawWifi {
            channel: wifi_channel,
            log_fn: None,
        }
    }

    pub fn set_channel(&mut self, wifi_channel: u8) {
        self.channel = wifi_channel;
    }

    pub fn channel(&self) -> u8 {
        self.channel
    }

    pub fn set_log(&mut self, log_fn: fn(&[u8])) {
        self.log_fn = Some(log_fn);
    }

    pub fn init_wifi(&self) {
        unsafe {
            let x = esp_wifi_set_ps(wifi_ps_type_t_WIFI_PS_NONE);
            let a =  esp_wifi_set_promiscuous_rx_cb(Some(raw_wifi_rx_cb));
            let v = esp_wifi_set_promiscuous(true);
            info!("rawwifi {} {} {}",x,a,v);

            let y = esp_wifi_set_channel(self.channel, wifi_second_chan_t_WIFI_SECOND_CHAN_NONE);
            info!("rawwifi2 {}",y);

        }
    }

    pub fn rx_cb(&self, pkt: Option<&mut wifi_promiscuous_pkt_t>, pkt_type: u32) {
        match pkt {
            None => {}
            Some(wifi_pkt) => {
                //if wifi_pkt.rx_ctrl.rate() <= 5 {
                    //info!("got packet of rate, {}",wifi_pkt.rx_ctrl.rate());
                    let size = wifi_pkt.rx_ctrl.sig_len() - 4;
                    unsafe {
                        let payload = wifi_pkt.payload.as_mut_slice(size as usize);
                        //info!("{:X?}", payload);
                        match self.log_fn {
                            Some(p) => p(payload),
                            None => {}
                        };
                    }
                //}
            }
        }
    }
}

unsafe extern "C" fn raw_wifi_rx_cb(pkt: *mut std::ffi::c_void, pkt_type: u32) {
    let raw_pkt: Option<&mut wifi_promiscuous_pkt_t> =
        (pkt as *mut wifi_promiscuous_pkt_t).as_mut();
    let inst = INSTANCE.get();
    //info!("rx_cb");
    inst.rx_cb(raw_pkt, pkt_type);
}
