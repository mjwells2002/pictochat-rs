pub mod packets;

pub mod ds_local_wifi_driver {
    use std::{hint, ptr, thread};
    use std::{
        collections::{HashMap, HashSet},
        sync::mpsc::{channel, Receiver, Sender},
        time::Duration,
    };
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::io::{Cursor, Read};
    use std::ops::Deref;
    use std::sync::{Arc, Mutex};
    use std::sync::mpsc::{sync_channel, SyncSender};
    use std::thread::sleep;
    use std::time::Instant;

    use bitflags::{bitflags, Flags};
    use byteorder::ReadBytesExt;
    use esp_idf_sys::{esp_task_wdt_reset, esp_wifi_80211_tx, esp_wifi_get_mac, esp_wifi_set_promiscuous, esp_wifi_set_promiscuous_rx_cb, esp_wifi_set_ps, esp_wifi_set_vendor_ie, TaskFunction_t, TaskHandle_t, tskTaskControlBlock, vTaskDelay, wifi_interface_t_WIFI_IF_AP, wifi_promiscuous_pkt_t, wifi_ps_type_t_WIFI_PS_NONE, wifi_vendor_ie_id_t_WIFI_VND_IE_ID_0, wifi_vendor_ie_type_t_WIFI_VND_IE_TYPE_BEACON, xTaskCreatePinnedToCore, xTaskGetCurrentTaskHandle};
    use ieee80211::{
        AssociationRequestFrame, AuthenticationFixedParametersTrait, DataFrame,
        DataFrameBuilderTrait, FragmentSequenceBuilderTrait, FrameBuilderTrait, FrameSubtype,
        FrameTrait, MacAddress, ManagementFrame, ManagementFrameBuilderTrait, ManagementFrameLayer,
        ManagementFrameTrait, ManagementSubtype, TaggedParametersTrait,
    };
    use ieee80211::{Frame, FrameLayer};
    use ieee80211::ControlFrameTrait;
    use ieee80211::DataFrameTrait;
    use log::{info, warn};
    use utf16string::LittleEndian;

    use crate::check_err;
    use crate::ds_local_wifi_driver::packets::packets::{BeaconType, DSWiFiBeaconTag};
    use crate::raw_wifi::INSTANCE;

    const CMD_MAC: MacAddress = MacAddress::new([0x03, 0x09, 0xbf, 0x00, 0x00, 0x00]);
    const ACK_MAC: MacAddress = MacAddress::new([0x03, 0x09, 0xbf, 0x00, 0x00, 0x03]);
    const CLIENT_ACK_MAC: MacAddress = MacAddress::new([0x03, 0x09, 0xbf, 0x00, 0x00, 0x10]);
    static mut WIFI_PKT_CHAN: Option<SyncSender<InboundPacket>> = None;

    #[derive(Clone)]
    pub enum ClientEvents {
        NewClient((MacAddress,u8)),
        LostClient((MacAddress,u8)),
    }


    pub trait LocalWifiLayer {
        fn get_local_mac(&self) -> MacAddress;
        fn get_console_hack(&self) -> MacAddress;
        fn set_broadcast_packet(
            &mut self,
            interval: Duration,
            packet: Vec<u8>,
            game_id: [u8; 4],
            beacon_type: BeaconType,
            cmd_size: u16,
            cmd_reply_size: u16,
        );
        fn set_broadcast_enabled(&mut self, enabled: bool);
        fn set_next_data_frame(&mut self, dataframe: HostToClientDataFrame);
    }

    pub trait LocalWifiApplicationLayer {
        fn setup(&mut self, upper_layer: &mut impl LocalWifiLayer);
        fn fill_next_packet(&mut self, upper_layer: &mut impl LocalWifiLayer);
        fn on_packet(&mut self, upper_layer: &mut impl LocalWifiLayer, data_frame: &mut ClientToHostDataFrame, device: &LocalWifiClientDevice);
        fn on_client_event(&mut self, client_event: ClientEvents);
    }

    pub struct LocalWifiDriver<T: LocalWifiApplicationLayer + Send> {
        next_layer: Option<T>,
        driver_inner: Option<LocalWifiDriverInner<T>>,

    }

    impl<T: LocalWifiApplicationLayer + Send + 'static> LocalWifiDriver<T> {
        pub fn new(next_layer: T) -> Self {
            LocalWifiDriver {
                next_layer: Some(next_layer),
                driver_inner: None,
            }
        }

        pub fn start(&mut self, local_mac: [u8;6]) -> (Receiver<Vec<u8>>, Sender<ClientEvents>) {
            let (wifi_rx, wifi_rx_to_thread) = sync_channel(1);
            let (wifi_tx_from_thread, wifi_tx) = channel();
            let (wifi_newclient, wifi_newclient_to_thread) = channel();
            let mut wifi_pending_rx: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
            let mut next_layer = self.next_layer.take().unwrap();

            /*thread::Builder::new().name("ds_wifi_processing_loop".to_string()).stack_size(16 * 1024).spawn(move || {
                let mut driver_inner =
                    LocalWifiDriverInner::new(wifi_rx_to_thread, wifi_tx_from_thread, next_layer, local_mac, wifi_newclient_to_thread);
                driver_inner.run();
            });*/

            unsafe {
                //let cloned_pending_rx = Arc::clone(&wifi_pending_rx);
                let raw_args_tuple:*mut Box<(Receiver<InboundPacket>,Sender<Vec<u8>>,T,[u8;6],Receiver<ClientEvents>)> = Box::into_raw(Box::new(Box::new((wifi_rx_to_thread,wifi_tx_from_thread,next_layer,local_mac,wifi_newclient_to_thread))));
                let mut task_handle = xTaskGetCurrentTaskHandle();

                xTaskCreatePinnedToCore(Some(ds_wifi_task_launchpad::<T>), CString::new("ds_wifi_task".to_string()).unwrap().into_bytes_with_nul().as_ptr() as *const c_char, 16 * 1024, raw_args_tuple as *mut c_void, 5, &mut task_handle, 1);

            }

            unsafe { WIFI_PKT_CHAN = Some(wifi_rx); }
            (wifi_tx, wifi_newclient )
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum InboundPacket {
        ACK((MacAddress, Instant, Instant)),
        DATA((MacAddress, Vec<u8>, Instant, Instant))
    }

    #[derive(Clone)]
    pub struct LocalWifiClientDevice {
        pub assoc_id: u16,
        pub assoc_id_num: u8,
        pub addr: MacAddress,
    }

    struct LocalWifiDriverInner<T: LocalWifiApplicationLayer + Send> {
        rx: Receiver<InboundPacket>,
        tx: Sender<Vec<u8>>,
        rxnc: Receiver<ClientEvents>,
        next_layer: Option<T>,
        last_seq: u16,
        local_mac: MacAddress,
        gbl_cmd_mac: MacAddress,
        gbl_ack_mac: MacAddress,
        gbl_client_ack_mac: MacAddress,
        gbl_stream_code: u16,
        broadcast_data: BroadcastData,
        client_map: HashMap<MacAddress, LocalWifiClientDevice>,
        curr_data_frame: Option<HostToClientDataFrame>,
        curr_assoc_id: u8,
        curr_data_duration: Duration,
        slot_1_duration: Duration,
        slot_2_duration: Duration,
        slot_3_duration: Duration,
        slot_4_duration: Duration,
        slot_2_packet: Option<Vec<u8>>,
        slot_3_packet: Option<Vec<u8>>,
        slot_4_packet: Option<Vec<u8>>,
        last_packet_send: Instant,
        last_packet_duration: Duration,
        all_client_mask: u16,
    }

    struct BroadcastData {
        enabled: bool,
        payload: Option<Vec<u8>>,
        interval: Duration,
        game_id: [u8; 4],
        beacon_type: BeaconType,
        cmd_size: u16,
        cmd_reply_size: u16,
    }

    pub struct ClientToHostDataFrame {
        pub payload_size: u16,
        pub flags: ClientToHostFlags,
        pub payload: Option<Vec<u8>>,
        pub footer_seq_no: Option<u16>,
    }

    impl ClientToHostDataFrame {
        pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
            if bytes.len() <= 2 {
                return None
            }
            let mut current = Cursor::new(bytes);
            let mut pkt_size: u16 = current.read_u8().unwrap() as u16 * 2;
            let cth_flags = ClientToHostFlags::from_bits(current.read_u8().unwrap()).unwrap();
            if (cth_flags.contains(ClientToHostFlags::LENGTH_IS_BYTES)) {
                pkt_size = pkt_size / 2;
            }
            let mut pkt_payload:Option<Vec<u8>> = None;
            if pkt_size != 0 {
                let mut pkt_bytes = vec![0u8; pkt_size as usize];
                match current.read_exact(&mut pkt_bytes) {
                    Ok(_) => {}
                    Err(_) => {
                        warn!("failed to read client to host packet");
                        return None
                    }
                }
                pkt_payload = Some(pkt_bytes);
            }
            Some(Self {
                payload_size: pkt_size,
                flags: cth_flags,
                payload: pkt_payload,
                footer_seq_no: if cth_flags.contains(ClientToHostFlags::HAS_FOOTER) { Some(current.read_u16::<LittleEndian>().unwrap()) } else { None }
            })
        }
    }

    // rx_time u16, rx_mask u16, halfword size u8, flags u8
    pub struct HostToClientDataFrame {
        pub us_per_client_reply: u16,
        pub rx_console_mask: u16,
        pub flags: HostToClientFlags,
        pub broadcast_cycles_remaining: u8, //starts at 4, will only go down when acks remaining is 0
        pub payload: Option<Vec<u8>>,
        pub footer: Option<HostToClientDataFrameFooter>,
    }

    #[derive(Clone)]
    pub struct HostToClientDataFrameFooter {
        pub seq_no: u16,
        pub idk: u16,
    }

    impl HostToClientDataFrameFooter {
        fn to_parts(&self) -> Vec<u8> {
            [self.seq_no.to_le_bytes(),self.idk.to_le_bytes()].concat()
        }
    }

    impl HostToClientDataFrame {
        fn to_bytes(&self) -> Vec<u8> {
            let rval: Vec<u8>;

            if self.footer.is_none() {
                if self.payload.is_none() {
                    rval = [
                        self.us_per_client_reply.to_le_bytes(),
                        self.rx_console_mask.to_le_bytes(),
                        [0x00, self.flags.bits()],
                    ]
                    .concat();
                } else {
                    let payload = self.payload.clone().unwrap();
                    rval = [
                        &self.us_per_client_reply.to_le_bytes(),
                        &self.rx_console_mask.to_le_bytes(),
                        &[(payload.len() / 2) as u8, self.flags.bits()],
                        payload.as_slice(),
                    ]
                    .concat();
                }
            } else {
                let footer = self.footer.clone().unwrap();
                if self.payload.is_none() {
                    rval = [
                        self.us_per_client_reply.to_le_bytes(),
                        self.rx_console_mask.to_le_bytes(),
                        [0x00, self.flags.bits()],
                        footer.seq_no.to_le_bytes(),
                        self.rx_console_mask.to_le_bytes()
                    ]
                    .concat();
                } else {
                    let payload = self.payload.clone().unwrap();
                    rval = [
                        &self.us_per_client_reply.to_le_bytes(),
                        &self.rx_console_mask.to_le_bytes(),
                        &[(payload.len() / 2) as u8, self.flags.bits()],
                        payload.as_slice(),
                        &footer.seq_no.to_le_bytes(),
                        &self.rx_console_mask.to_le_bytes()
                    ]
                    .concat();
                }
            }

            rval
        }


    }

    /*#[repr(u8)]
    #[derive(Copy, Clone)]
    pub enum HostToClientFlags {
        NONE = 0,
        RESERVED_0 = 1 << 0,
        RESERVED_1 = 1 << 1,
        RESERVED_2 = 1 << 2,
        HAS_FOOTER = 1 << 3,
        RESERVED_4 = 1 << 4,
        RESERVED_5 = 1 << 5,
        RESERVED_6 = 1 << 6,
        RESERVED_7 = 1 << 7,
    }*/


    bitflags! {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct HostToClientFlags: u8 {
            const RESERVED_0 = 1 << 0;
            const RESERVED_1 = 1 << 1;
            const RESERVED_2 = 1 << 2;
            const HAS_FOOTER = 1 << 3;
            const RESERVED_4 = 1 << 4;
            const RESERVED_5 = 1 << 5;
            const RESERVED_6 = 1 << 6;
            const NEXT_TRANSACTION_DELAYED = 1 << 7;
        }
    }

    bitflags! {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct ClientToHostFlags: u8 {
            const RESERVED_0 = 1 << 0;
            const RESERVED_1 = 1 << 1;
            const RESERVED_2 = 1 << 2;
            const HAS_FOOTER = 1 << 3;
            const RESERVED_4 = 1 << 4;
            const LENGTH_IS_BYTES = 1 << 5;
            const RESERVED_6 = 1 << 6;
            const RESERVED_7 = 1 << 7;
        }
    }

    impl Default for HostToClientDataFrame {
        fn default() -> Self {
            return HostToClientDataFrame {
                us_per_client_reply: 990,
                rx_console_mask: 0,
                flags: HostToClientFlags::from_bits(0).unwrap(),
                broadcast_cycles_remaining: 4,
                payload: None,
                footer: None
            };
        }
    }

    impl<T: LocalWifiApplicationLayer + Send> LocalWifiDriverInner<T> {
        fn new(wifi_pending_packet: Receiver<InboundPacket>, tx_channel: Sender<Vec<u8>>, next_layer: T, local_mac: [u8;6], newclient_channel: Receiver<ClientEvents> ) -> Self {
            LocalWifiDriverInner {
                next_layer: Some(next_layer),
                rx: wifi_pending_packet,
                tx: tx_channel,
                rxnc: newclient_channel,
                last_seq: 0,
                local_mac: MacAddress::new(local_mac.into()),
                gbl_cmd_mac: MacAddress::new([0x03, 0x09, 0xbf, 0x00, 0x00, 0x00].into()),
                gbl_ack_mac: MacAddress::new([0x03, 0x09, 0xbf, 0x00, 0x00, 0x03].into()),
                gbl_client_ack_mac: MacAddress::new([0x03, 0x09, 0xbf, 0x00, 0x00, 0x10].into()),
                gbl_stream_code: 1,
                broadcast_data: BroadcastData {
                    enabled: false,
                    payload: None,
                    interval: Duration::from_millis(0),
                    game_id: [0x00, 0x00, 0xff, 0x00],
                    beacon_type: BeaconType::EMPTY,
                    cmd_size: 00,
                    cmd_reply_size: 00,
                },
                client_map: HashMap::new(),
                curr_data_frame: Some(HostToClientDataFrame {
                    us_per_client_reply: 998,
                    rx_console_mask: 0,
                    flags: HostToClientFlags::empty(),
                    broadcast_cycles_remaining: 1,
                    payload: None,
                    footer: None,
                }),
                curr_assoc_id: 1, //0 refers to the host console,
                curr_data_duration: Duration::from_millis(5),
                slot_1_duration: Duration::from_millis(100),
                slot_2_duration: Duration::from_micros(3000),
                slot_3_duration: Duration::from_micros(3000),
                slot_4_duration: Duration::from_micros(3000),
                slot_2_packet: None,
                slot_3_packet: None,
                slot_4_packet: None,
                last_packet_send: Instant::now(),
                last_packet_duration: Duration::from_millis(0),
                all_client_mask: 0,

            }
        }

        /*fn run(&mut self) {
            let mut next_layer = self.next_layer.take().unwrap();
            next_layer.setup(self);
            self.next_layer = Some(next_layer);

            let mut last_data = Instant::now();
            let mut last_stream_code = Instant::now();
            let mut last_beacon = Instant::now();
            let first_init = Instant::now();
            loop {
                match self.rx.recv_timeout(Duration::from_micros(100)) {
                    Ok(pkt) => self.on_packet(&pkt),
                    Err(_) => {}
                }

                if self.client_map.len() > 0 && last_data.elapsed() >= self.curr_data_duration {
                    last_data = Instant::now();
                    self.send_current_data_frame();
                }

                if self.broadcast_data.enabled
                    && last_beacon.elapsed() >= self.broadcast_data.interval
                {
                    last_beacon = Instant::now();
                    self.send_beacon(first_init.elapsed());
                }

                if self.broadcast_data.enabled
                    && last_stream_code.elapsed() >= Duration::from_millis(5000)
                {
                    //increase stream code every 5 seconds
                    self.gbl_stream_code = self.gbl_stream_code.wrapping_add(1);
                    last_stream_code = Instant::now();
                }
            }
        }*/

        fn run(&mut self) {
            unsafe {
                let x = esp_wifi_set_ps(wifi_ps_type_t_WIFI_PS_NONE);
                let a =  esp_wifi_set_promiscuous_rx_cb(Some(special_wifi_rx_cb));
                let v = esp_wifi_set_promiscuous(true);
            }

            let mut next_layer = self.next_layer.take().unwrap();
            next_layer.setup(self);
            self.next_layer = Some(next_layer);
            let mut latency_avg = Vec::<u16>::with_capacity(100);
            let mut latency_avg_2 = Vec::<u16>::with_capacity(100);
            let mut latency_avg_3 = Vec::<u16>::with_capacity(100);

            loop {
                unsafe {
                    vTaskDelay(1);
                }

                match self.rxnc.recv_timeout(Duration::from_micros(10)) {
                    Ok(evt) => {
                        //TODO: assoc id
                        match evt {
                            ClientEvents::NewClient((mac,aid)) => {
                                info!("client has connected, adding client to client map");
                                self.client_map.insert(
                                    mac.clone(),
                                    LocalWifiClientDevice {
                                        assoc_id: 0x0001 << aid,
                                        assoc_id_num: aid,
                                        addr: mac.clone(),
                                    },
                                );
                                self.all_client_mask |= 0x0001 << aid;
                            }
                            ClientEvents::LostClient((mac, aid)) => {
                                info!("client has disconnected, removing from client map");
                                self.client_map.remove(&mac.clone());
                                self.all_client_mask &= !(0x0001 << aid);
                            }
                        }
                        let mut next_layer = self.next_layer.take().unwrap();
                        next_layer.on_client_event(evt);
                        self.next_layer = Some(next_layer);

                    },
                    Err(_) => {}
                }
                let mut rx_pktc = 0;

                for _ in 0..self.client_map.len() {
                    match self.rx.recv_timeout(Duration::from_micros(100)) {
                        Ok(pkt) => {
                            let (l1,_) = self.on_packet_real(&pkt);
                            latency_avg.push(l1);
                           // latency_avg_3.push(l2);
                            rx_pktc += 1;
                        },
                        Err(_) => {}
                    }
                }

                if latency_avg.len() >= 100 {
                    latency_avg.sort_unstable();
                    let sum: u16 = latency_avg.iter().sum();
                    let count = latency_avg.len();
                    let average = sum as f32 / count as f32;
                    let p99 = latency_avg[(0.99 * count as f32) as usize];
                    let p95 = latency_avg[(0.95 * count as f32) as usize];
                    info!("Min {}, Max {}, Avg {}, P99 {}, P95 {}",latency_avg[0], *latency_avg.last().unwrap(), average, p99, p95);
                    latency_avg.clear();
                }

                //info!("rx pktc {}", rx_pktc);

                if self.client_map.len() > 0 {

                    self.send_current_data_frame()
                }


                let mut data_frame = self.curr_data_frame.take().unwrap();
                let console_mask = data_frame.rx_console_mask.clone();
                self.curr_data_frame = Some(data_frame);

                let now = Instant::now();
                while now.elapsed() <= Duration::from_micros(10_000) {
                    //fuck it spinlock time
                    if console_mask != 0 {
                        match self.rx.recv_timeout(Duration::from_micros(10)) {
                            Ok(pkt) => {
                                let (l1,l2) = self.on_packet_real(&pkt);
                                latency_avg_2.push(l1);
                                latency_avg_3.push(l2)
                            },
                            Err(_) => {}
                        }
                    } else {
                        break;
                    }
                }
                if latency_avg_2.len() >= 100 {
                    latency_avg_2.sort_unstable();
                    let sum: u16 = latency_avg_2.iter().sum();
                    let count = latency_avg_2.len();
                    let average = sum as f32 / count as f32;
                    let p99 = latency_avg_2[(0.99 * count as f32) as usize];
                    let p95 = latency_avg_2[(0.95 * count as f32) as usize];
                    info!("(2) Min {}, Max {}, Avg {}, P99 {}, P95 {}",latency_avg_2[0], *latency_avg_2.last().unwrap(), average, p99, p95);
                    latency_avg_2.clear();
                }
                if latency_avg_3.len() >= 100 {
                    latency_avg_3.sort_unstable();
                    let sum: u16 = latency_avg_3.iter().sum();
                    let count = latency_avg_3.len();
                    let average = sum as f32 / count as f32;
                    let p99 = latency_avg_3[(0.99 * count as f32) as usize];
                    let p95 = latency_avg_3[(0.95 * count as f32) as usize];
                    info!("(2) Min {}, Max {}, Avg {}, P99 {}, P95 {}",latency_avg_3[0], *latency_avg_3.last().unwrap(), average, p99, p95);
                    latency_avg_3.clear();
                }
            }
        }

        fn on_packet_real(&mut self, data: &InboundPacket) -> (u16,u16) {
            match data {
                InboundPacket::ACK((mac,send_time,rx_time)) => {
                    let mut frame = self.curr_data_frame.take().unwrap();
                    let client = self.client_map.get(&mac).unwrap().clone();

                    frame.rx_console_mask &= !client.assoc_id;
                    if frame.rx_console_mask == 0 {
                        self.send_hardware_ack_frame()
                    }
                    self.curr_data_frame = Some(frame);
                    //info!("channel ack latency {}", send_time.elapsed().as_micros());
                    (send_time.elapsed().as_micros() as u16,rx_time.elapsed().as_micros() as u16)
                }
                InboundPacket::DATA((mac,payload,send_time,rx_time)) => {
                    let mut frame = self.curr_data_frame.take().unwrap();
                    let client = self.client_map.get(&mac).unwrap().clone();

                    frame.rx_console_mask &= !client.assoc_id;
                    if frame.rx_console_mask == 0 {
                        self.send_hardware_ack_frame()
                    }

                    self.curr_data_frame = Some(frame);
                    match ClientToHostDataFrame::from_bytes(payload) {
                        None => {}
                        Some(mut client_to_host_frame) => {
                            if client_to_host_frame.payload.is_some() {
                                let mut layer = self.next_layer.take().unwrap();
                                layer.on_packet(self,&mut client_to_host_frame,&client);
                                self.next_layer = Some(layer);

                            }
                        }
                    }
                    (send_time.elapsed().as_micros() as u16,rx_time.elapsed().as_micros() as u16)
                }
            }
            /*let frame = Frame::new(data);
            let layer = frame.next_layer().unwrap();
            match layer {
                FrameLayer::Management(ref management_frame) => {
                    //self.on_mgmt_frame(management_frame);
                }
                FrameLayer::Control(ref control_frame) => {
                    info!("Control frame {:?}", control_frame.transmitter_address());
                }
                FrameLayer::Data(ref data_frame) => {
                    self.on_data_frame(data_frame);
                }
            };*/
        }

        fn on_data_frame(&mut self, data: &DataFrame) {
            let subtype = data.subtype();
            match subtype {
                FrameSubtype::Data(ieee80211::DataSubtype::DataCFAck) => {
                    if data.destination_address().unwrap() == self.gbl_client_ack_mac {
                        //info!("PKT");
                        let src_addr = data.source_address().unwrap();
                        let client = self.client_map.get(&src_addr).unwrap().clone();


                        let remaining_data = data.next_layer();

                        match remaining_data {
                            None => {}
                            Some(pkt_data) => {
                                match ClientToHostDataFrame::from_bytes(pkt_data) {
                                    None => {}
                                    Some(mut client_to_host_frame) => {
                                        if client_to_host_frame.payload.is_some() {
                                            let mut layer = self.next_layer.take().unwrap();
                                            layer.on_packet(self,&mut client_to_host_frame,&client);
                                            self.next_layer = Some(layer);
                                        }
                                    }
                                }
                            }
                        }


                    }
                }

                FrameSubtype::Data(ieee80211::DataSubtype::CFAck) => {
                    if data.destination_address().unwrap() == self.gbl_client_ack_mac {
                        let src_addr = data.source_address().unwrap();
                        let client = self.client_map.get(&src_addr).unwrap();
                        let mut frame = self.curr_data_frame.take().unwrap();
                        frame.rx_console_mask &= !client.assoc_id;
                        //self.send_hardware_ack_frame();
                        self.curr_data_frame = Some(frame);
                    }
                }

                _default => (),
            }
        }

        /*fn on_mgmt_frame(&mut self, data: &ManagementFrame) {
            //todo: use proper packet structures here for the auth response
            let management_frame_layer = data.next_layer().unwrap();

            if let ManagementFrameLayer::Authentication(ref auth_frame) = management_frame_layer {
                match auth_frame.status_code() {
                    ieee80211::StatusCode::Successful => {
                        if self.slot_2_packet.is_none() {
                            info!("Got auth frame from {:?}", data.transmitter_address());
                            let mut frameBuilder = ieee80211::ManagementFrameBuilder::new_blank();
                            frameBuilder
                                .subtype(FrameSubtype::Management(ManagementSubtype::Authentication));
                            //frameBuilder.duration_or_id(ieee80211::DurationID::Reserved(0));
                            frameBuilder.receiver_address(data.transmitter_address().unwrap());
                            frameBuilder.destination_address(data.transmitter_address().unwrap());
                            frameBuilder.transmitter_address(self.local_mac);
                            frameBuilder.bssid_address(self.local_mac);
                            frameBuilder.fragment_number(0);
                            frameBuilder.sequence_number(self.last_seq);
                            frameBuilder.ds_status(ieee80211::DSStatus::NotLeavingDSOrADHOC);
                            let params: [u8; 6] = [0x00, 0x00, 0x02, 0x00, 0x00, 0x00];
                            let data = frameBuilder.build();
                            let pkt = [&[0xffu8], data.bytes(), &params].concat();
                            //self.tx.send(pkt);
                            self.slot_2_packet = Some(pkt);
                            self.last_seq += 1;
                            info!("Adding auth frame for {:?} to slot 2", data.transmitter_address());
                        }

                    }
                default => {}
                }
            } else if let ManagementFrameLayer::AssociationRequest(ref auth_frame) =
                management_frame_layer
            {
                info!(
                    "Got association request frame from {:?}, {:?}",
                    data.transmitter_address(),
                    auth_frame.ssid()
                );
                self.handle_assoc_request_frame(auth_frame);
            }
        }*/

        /*fn handle_assoc_request_frame(&mut self, auth_frame: &AssociationRequestFrame) {
            //TODO: check the ssid provided matches, to avoid game/stream conflicts
            if self.slot_2_packet.is_none() {
                let mut frameBuilder = ieee80211::ManagementFrameBuilder::new_blank();
                frameBuilder.subtype(FrameSubtype::Management(
                    ManagementSubtype::AssociationResponse,
                ));
                //frameBuilder.duration_or_id(ieee80211::DurationID::Reserved(0));
                frameBuilder.receiver_address(auth_frame.transmitter_address().unwrap());
                frameBuilder.destination_address(auth_frame.transmitter_address().unwrap());
                frameBuilder.transmitter_address(self.local_mac);
                frameBuilder.bssid_address(self.local_mac);
                frameBuilder.fragment_number(0);
                frameBuilder.sequence_number(self.last_seq);
                frameBuilder.ds_status(ieee80211::DSStatus::NotLeavingDSOrADHOC);
                let params: [u8; 3] = [0x21, 0x00, 0x00];
                let params_pt2: [u8; 5] = [0xc0, 0x01, 0x02, 0x82, 0x84];
                //self.curr_assoc_id += 1;
                let assoc_id: u16 = 0x0001 << self.curr_assoc_id;
                let data = frameBuilder.build();
                let pkt = [
                    &[0xffu8],
                    data.bytes(),
                    &params,
                    &(self.curr_assoc_id as u16).to_be_bytes(),
                    &params_pt2,
                ].concat();
                self.client_map.insert(
                    auth_frame.source_address().unwrap().clone(),
                    LocalWifiClientDevice {
                        assoc_id: assoc_id,
                        assoc_id_num: self.curr_assoc_id,
                        addr: auth_frame.source_address().unwrap().clone(),
                    },
                );
                self.slot_2_packet = Some(pkt);
                self.last_seq += 1;
                info!("Adding association response frame for  {:?} association id {} to slot 2",
                    auth_frame.source_address(),self.curr_assoc_id);
            }

        }*/

        /*fn send_beacon(&mut self, init_time: Duration) {
            let mut frameBuilder = ieee80211::ManagementFrameBuilder::new_blank();
            frameBuilder.subtype(ieee80211::FrameSubtype::Reserved(0, 8));
            // frameBuilder.duration_or_id(ieee80211::DurationID::Duration(1282));
            frameBuilder.receiver_address(MacAddress::broadcast());
            frameBuilder.destination_address(MacAddress::broadcast());
            frameBuilder.transmitter_address(self.local_mac);
            frameBuilder.bssid_address(self.local_mac);
            frameBuilder.fragment_number(0);
            frameBuilder.sequence_number(self.last_seq);
            frameBuilder.ds_status(ieee80211::DSStatus::NotLeavingDSOrADHOC);

            let microseconds_since_start = init_time.as_micros() as u64;

            let params: [u8; 18] = [
                0x6a, 0x00, 0x21, 0x00, 0x01, 0x02, 0x82, 0x84, 0x03, 0x01, 0x07, 0x05, 0x05, 0x01,
                0x02, 0x00, 0x00, 0x00,
            ];
            let data = frameBuilder.build();

            let mut beacon = DSWiFiBeaconTag {
                game_id: self.broadcast_data.game_id,
                beacon_type: self.broadcast_data.beacon_type,
                cmd_data_size: self.broadcast_data.cmd_size,
                reply_data_size: self.broadcast_data.cmd_reply_size,
                stream_code: self.gbl_stream_code,
                ..Default::default()
            };
            let r_payload = self.broadcast_data.payload.take();
            let payload = r_payload.clone();
            self.broadcast_data.payload = r_payload;
            let beacon_tag = beacon.to_bytes(payload);

            let pkt = [
                &[0x00u8],
                data.bytes(),
                &microseconds_since_start.to_le_bytes(),
                &params,
                &beacon_tag,
            ]
            .concat();
            //self.tx.send(pkt);
            self.last_seq = self.last_seq.wrapping_add(1);
        }*/

        fn send_current_data_frame(&mut self) {
            let mut cur_frame = self.curr_data_frame.take().unwrap();

            let mut next_layer = self.next_layer.take().unwrap();


            if cur_frame.rx_console_mask == 0 {
                //if cur_frame.flags.contains(HostToClientFlags::NEXT_TRANSACTION_DELAYED) {
                    //sleep(Duration::from_millis(10));
                //}
                self.curr_data_frame = None;
                next_layer.fill_next_packet(self);
                cur_frame = self.curr_data_frame.take().unwrap();
            }

            self.next_layer = Some(next_layer);

            let bytes = cur_frame.to_bytes();

            let mut frameBuilder = ieee80211::DataFrameBuilder::new();
            frameBuilder.subtype(ieee80211::FrameSubtype::Data(
                ieee80211::DataSubtype::DataCFPoll,
            ));
            //frameBuilder.duration_or_id(ieee80211::DurationID::Reserved(0));
            frameBuilder.receiver_address(self.gbl_cmd_mac);
            frameBuilder.destination_address(self.gbl_cmd_mac);
            frameBuilder.transmitter_address(self.local_mac);
            frameBuilder.bssid_address(self.local_mac);
            frameBuilder.fragment_number(0);
            frameBuilder.sequence_number(self.last_seq);
            //frameBuilder.ds_status(ieee80211::DSStatus::NotLeavingDSOrADHOC);
            frameBuilder.ds_status(ieee80211::DSStatus::FromDSToSTA);
            //frameBuilder.duration_or_id(ieee80211::DurationID::Duration(1248));

            let frame = frameBuilder.build();
            let mut net_bytes = [frame.bytes(), &bytes].concat();
            if (cur_frame.rx_console_mask != 0) {
                net_bytes[2] = 0xe0;
                net_bytes[3] = 0x04;
            } else {
                net_bytes[2] = 0xf0;
                net_bytes[3] = 0x00;
            }
            unsafe {

                while self.last_packet_send.elapsed() < self.last_packet_duration {
                    //spinlock
                    sleep(Duration::from_micros(100));
                }
                self.last_packet_send = Instant::now();
                self.last_packet_duration = Duration::from_micros(3200);

                esp_wifi_80211_tx(
                    wifi_interface_t_WIFI_IF_AP,
                    net_bytes.as_ptr() as *const c_void,
                    net_bytes.len() as c_int,
                    true,
                );
            };
            self.last_seq = self.last_seq.wrapping_add(1);

            //lazy trick, to bypass not actually listening for client ack
            //TODO: fixme
            if (cur_frame.rx_console_mask == 0) {
                self.send_hardware_ack_frame();
                cur_frame.broadcast_cycles_remaining -= 1;
                self.curr_data_duration = Duration::from_micros(2800);
            } else {
                self.curr_data_duration = Duration::from_micros(6800);
            }

            //cur_frame.rx_console_mask = 0x0000;
            self.curr_data_frame = Some(cur_frame);


        }

        fn send_hardware_ack_frame(&mut self) {
            let mut frameBuilder = ieee80211::DataFrameBuilder::new();
            frameBuilder.subtype(FrameSubtype::Data(ieee80211::DataSubtype::DataCFAck));
            frameBuilder.receiver_address(self.gbl_ack_mac);
            frameBuilder.source_address(self.gbl_ack_mac);
            frameBuilder.transmitter_address(self.local_mac);
            frameBuilder.bssid_address(self.local_mac);
            frameBuilder.fragment_number(0);
            frameBuilder.sequence_number(self.last_seq);
            frameBuilder.ds_status(ieee80211::DSStatus::FromDSToSTA);
            //frameBuilder.duration_or_id(ieee80211::DurationID::Duration(1248));

            let payload: [u8; 4] = [0x33, 0x00, 0x00, 0x00]; //todo: i think there is a bitmask here for errored consoles
            let data = frameBuilder.build();
            let pkt = [data.bytes(), &payload].concat();
            //self.tx.send(pkt);
            let mut res = 1;
            unsafe {
                //while res != 0 {
                    res = esp_wifi_80211_tx(
                        wifi_interface_t_WIFI_IF_AP,
                        pkt.as_ptr() as *const c_void,
                        pkt.len() as c_int,
                        true,
                    )
                //}
            }

            /*if (data[0] == 0xff) {
                rom_phy_enable_cca();
            }*/
            if res != 0 {
                //info!("packet send failed, {}", res);
            }

            self.last_seq = self.last_seq.wrapping_add(1);
        }
    }

    impl<T: LocalWifiApplicationLayer + Send> LocalWifiLayer for LocalWifiDriverInner<T> {
        fn get_local_mac(&self) -> MacAddress {
            return self.local_mac;
        }

        fn get_console_hack(&self) -> MacAddress {
            let data: Vec<MacAddress> = self.client_map.keys().cloned().collect();
            return *(data.get(0).unwrap());
        }

        fn set_broadcast_packet(
            &mut self,
            interval: Duration,
            packet: Vec<u8>,
            game_id: [u8; 4],
            beacon_type: BeaconType,
            cmd_size: u16,
            cmd_reply_size: u16,
        ) {
            self.broadcast_data.payload = Some(packet);
            self.broadcast_data.interval = interval;
            self.broadcast_data.game_id = game_id;
            self.broadcast_data.beacon_type = beacon_type;
            self.broadcast_data.cmd_size = cmd_size;
            self.broadcast_data.cmd_reply_size = cmd_reply_size;
        }

        fn set_broadcast_enabled(&mut self, enabled: bool) {
            //self.broadcast_data.enabled = enabled;
            let mut beacon = DSWiFiBeaconTag {
                game_id: self.broadcast_data.game_id,
                beacon_type: self.broadcast_data.beacon_type,
                cmd_data_size: self.broadcast_data.cmd_size,
                reply_data_size: self.broadcast_data.cmd_reply_size,
                stream_code: self.gbl_stream_code,
                ..Default::default()
            };
            let r_payload = self.broadcast_data.payload.take();
            let payload = r_payload.clone();
            self.broadcast_data.payload = r_payload;

            unsafe {
                esp_wifi_set_vendor_ie(enabled, wifi_vendor_ie_type_t_WIFI_VND_IE_TYPE_BEACON, wifi_vendor_ie_id_t_WIFI_VND_IE_ID_0, beacon.to_bytes(payload).leak().as_ptr() as *const c_void);
            }
        }

        fn set_next_data_frame(&mut self, mut dataframe: HostToClientDataFrame) {
            let payload = dataframe.payload.clone().unwrap();
            if (payload.len() % 2) != 0 {
                panic!("packet size not even {}",payload.len())
            }
            //info!("Dataframe set mask: {}",self.all_client_mask);
            dataframe.rx_console_mask = self.all_client_mask;
            self.curr_data_frame = Some(dataframe);
        }
    }

    pub struct DummyLocalWifiApplicationLayer {}

    impl LocalWifiApplicationLayer for DummyLocalWifiApplicationLayer {
        fn setup(&mut self, upper_layer: &mut impl LocalWifiLayer) {}
        fn fill_next_packet(&mut self, upper_layer: &mut impl LocalWifiLayer) {}
        fn on_packet(&mut self, upper_layer: &mut impl LocalWifiLayer, data_frame: &mut ClientToHostDataFrame, device: &LocalWifiClientDevice) {}
        fn on_client_event(&mut self, client_event: ClientEvents) {}
    }

    unsafe extern "C" fn ds_wifi_task_launchpad<T: LocalWifiApplicationLayer + Send + 'static>(ptr: *mut ::core::ffi::c_void) {
        info!("rust_callback_launchpad 1");
        let (wifi_rx_to_thread, wifi_tx_from_thread, next_layer, local_mac, wifi_newclient_to_thread) = **Box::from_raw(ptr as *mut Box<(Receiver<InboundPacket>,Sender<Vec<u8>>,T,[u8;6],Receiver<ClientEvents>)>);
        let mut driver_inner =
            LocalWifiDriverInner::new(wifi_rx_to_thread, wifi_tx_from_thread, next_layer, local_mac, wifi_newclient_to_thread);
        driver_inner.run();
    }

    unsafe extern "C" fn special_wifi_rx_cb(pkt: *mut std::ffi::c_void, pkt_type: u32) {
        let pkt_rx_time = Instant::now();
        let raw_pkt: Option<&mut wifi_promiscuous_pkt_t> = (pkt as *mut wifi_promiscuous_pkt_t).as_mut();
        if let wifi_pkt = raw_pkt.unwrap() {
            let size = wifi_pkt.rx_ctrl.sig_len() - 4;
            unsafe {
                let payload = wifi_pkt.payload.as_mut_slice(size as usize);
                let frame = Frame::new(payload as &[u8]);
                if let FrameLayer::Data(ref data) = frame.next_layer().unwrap() {
                    match data.subtype() {
                        FrameSubtype::Data(ieee80211::DataSubtype::DataCFAck) => {
                            if data.destination_address().unwrap() == CLIENT_ACK_MAC {
                                let src_addr = data.source_address().unwrap();

                                let mut frame_builder = ieee80211::DataFrameBuilder::new();

                                let mut mac: [u8; 6] = [0u8, 0u8, 0u8, 0u8, 0u8, 0u8];
                                esp_wifi_get_mac(wifi_interface_t_WIFI_IF_AP, mac.as_mut_ptr());

                                frame_builder.subtype(FrameSubtype::Data(ieee80211::DataSubtype::DataCFAck));
                                frame_builder.receiver_address(ACK_MAC);
                                frame_builder.source_address(ACK_MAC);
                                frame_builder.transmitter_address(MacAddress::from_bytes(&mac).unwrap());
                                frame_builder.bssid_address(MacAddress::from_bytes(&mac).unwrap());
                                frame_builder.fragment_number(0);
                                frame_builder.sequence_number(0);
                                frame_builder.ds_status(ieee80211::DSStatus::FromDSToSTA);
                                frame_builder.next_layer(&[0x33, 0x00, 0x00, 0x00]);

                                let data_tosend = frame_builder.build();
                                let pkt = data_tosend.bytes();
                                let mut res = 1;
                                let timer = Instant::now();
                                unsafe {
                                    //while res != 0 && timer.elapsed() < Duration::from_millis(5) {
                                        //res = esp_wifi_80211_tx(
                                        //    wifi_interface_t_WIFI_IF_AP,
                                        //    pkt.as_ptr() as *const c_void,
                                        //    pkt.len() as c_int,
                                        //    true,
                                        //);
                                    //}
                                }

                                if res != 0 {
                                    //info!("packet send failed, {}", res);
                                }

                                match data.next_layer() {
                                    None => {
                                        let channel = WIFI_PKT_CHAN.take().unwrap();
                                        channel.send(InboundPacket::ACK((
                                            src_addr,Instant::now(),pkt_rx_time
                                        ))).expect("failed to send packet to upper driver");
                                        WIFI_PKT_CHAN = Some(channel)
                                    }
                                    Some(payload) => {
                                        let channel = WIFI_PKT_CHAN.take().unwrap();
                                        channel.send(InboundPacket::DATA((
                                            src_addr,payload.to_vec(),Instant::now(),pkt_rx_time
                                        ))).expect("failed to send packet to upper driver");
                                        WIFI_PKT_CHAN = Some(channel)
                                    }
                                }

                            }
                        }
                        FrameSubtype::Data(ieee80211::DataSubtype::CFAck) => {
                            if data.destination_address().unwrap() == CLIENT_ACK_MAC {
                                let src_addr = data.source_address().unwrap();
                                let mut frame_builder = ieee80211::DataFrameBuilder::new();

                                let mut mac: [u8; 6] = [0u8, 0u8, 0u8, 0u8, 0u8, 0u8];
                                esp_wifi_get_mac(wifi_interface_t_WIFI_IF_AP, mac.as_mut_ptr());

                                frame_builder.subtype(FrameSubtype::Data(ieee80211::DataSubtype::DataCFAck));
                                frame_builder.receiver_address(ACK_MAC);
                                frame_builder.source_address(ACK_MAC);
                                frame_builder.transmitter_address(MacAddress::from_bytes(&mac).unwrap());
                                frame_builder.bssid_address(MacAddress::from_bytes(&mac).unwrap());
                                frame_builder.fragment_number(0);
                                frame_builder.sequence_number(0);
                                frame_builder.ds_status(ieee80211::DSStatus::FromDSToSTA);
                                frame_builder.next_layer(&[0x33, 0x00, 0x00, 0x00]);

                                let data = frame_builder.build();
                                let pkt = data.bytes();
                                let mut res = 1;
                                let timer = Instant::now();

                                //while res != 0 && timer.elapsed() < Duration::from_millis(5){
                                    //res = esp_wifi_80211_tx(
                                    //    wifi_interface_t_WIFI_IF_AP,
                                    //    pkt.as_ptr() as *const c_void,
                                    //    pkt.len() as c_int,
                                    //    true,
                                    //);
                                //}

                                if res != 0 {
                                    //info!("packet send failed, {}", res);
                                }

                                let channel = WIFI_PKT_CHAN.take().unwrap();
                                channel.send(InboundPacket::ACK((
                                    src_addr,Instant::now(),pkt_rx_time
                                ))).expect("failed to send packet to upper driver");
                                WIFI_PKT_CHAN = Some(channel)
                            }
                        }

                        _default => (),
                    }
                    /*if data.destination_address().unwrap() == CLIENT_ACK_MAC ||
                        data.destination_address().unwrap() == ACK_MAC ||
                        data.destination_address().unwrap() == CMD_MAC {
                        if (WIFI_PKT_CHAN.is_none()) {
                            return;
                        }
                        let channel = WIFI_PKT_CHAN.take().unwrap();
                        let mut npkt = Vec::new();
                        npkt.extend_from_slice(payload);
                        channel.send(npkt).expect("failed to send packet to upper driver");
                        //channel.send(npkt);
                        WIFI_PKT_CHAN = Some(channel)
                    }*/
                };



                /*let mut time = Instant::now();
                while time.elapsed() < Duration::from_millis(5) {
                    let mut mutex_guard = channel.lock().unwrap();
                    let pending_packet = mutex_guard.take();
                    if pending_packet.is_some() {
                        continue;
                    } else {
                        let mut npkt = Vec::new();
                        npkt.extend_from_slice(payload);
                        *mutex_guard = Some(npkt);
                        std::mem::drop(mutex_guard);
                        break;
                    }
                    std::mem::drop(mutex_guard);
                }
                if (time.elapsed().as_micros() > 1000) {
                    info!("pkt send time: {}",time.elapsed().as_micros());
                }*/

            }

        }

    }
}

