#[path = "../ds_local_wifi_driver/mod.rs"]
mod ds_local_wifi_driver;

mod pictochat_packets;

pub mod pictochat_app_driver {
    use std::borrow::Cow;
    use std::collections::{HashMap, VecDeque};
    use std::fmt;
    use std::io::{Cursor, Read};
    use std::sync::{Arc, Mutex};
    use std::sync::mpsc::{Receiver, SyncSender};
    use std::time::{Duration, Instant};

    use bitflags::Flags;
    use byteorder::{LittleEndian, ReadBytesExt};
    use esp_idf_hal::gpio::{PinDriver, Pull};
    use esp_idf_hal::peripherals::Peripherals;
    use ieee80211::MacAddress;
    use log::info;
    use rand::{random, Rng, RngCore, thread_rng};
    use serde::{Deserialize, Serialize};
    use utf16string::WString;

    use crate::{BAD_APPLE, gbl_button};
    use crate::ds_local_wifi_driver::ds_local_wifi_driver::{ClientEvents, ClientToHostDataFrame, HostToClientDataFrame, HostToClientDataFrameFooter, LocalWifiClientDevice};
    use crate::ds_local_wifi_driver::ds_local_wifi_driver::HostToClientFlags;
    use crate::ds_local_wifi_driver::ds_local_wifi_driver::LocalWifiApplicationLayer;
    use crate::ds_local_wifi_driver::ds_local_wifi_driver::LocalWifiLayer;
    use crate::ds_local_wifi_driver::packets::packets::BeaconType;
    use crate::pictochat_app_driver::pictochat_app_driver::PictochatAppState::Idle;
    use crate::pictochat_app_driver::pictochat_packets::pictochat_packets::*;

    const MAX_PACKET_ALLOC_SIZE: u16 = 16*1024;
    const MESSAGE_CHUNK_SIZE: u8 = 180;


    #[derive(PartialEq, Eq, Hash, Debug)]
    pub struct WStringWrapper(pub(crate) WString<LittleEndian>);

    impl Clone for WStringWrapper {
        fn clone(&self) -> Self {
            unsafe { Self(WString::from_utf16le_unchecked(self.0.as_bytes().clone().to_vec())) }
        }
    }

    impl From<WString<LittleEndian>> for WStringWrapper {
        fn from(s: WString<LittleEndian>) -> Self {
            Self(s)
        }
    }


    #[derive(Debug, PartialEq)]
    pub enum PictochatEvent {
        STATE(PictochatUserStateEvent),
        MESSAGE(PictochatUserMessageEvent),
    }

    #[derive(Debug, PartialEq)]
    pub struct PictochatUserStateEvent {
        pub mac_address: MacAddress,
        pub is_leaving: bool,
        pub birthday: u8,
        pub birthmonth: u8,
        pub name: WString<LittleEndian>,
        pub bio: WString<LittleEndian>,
    }

    #[derive(Debug, PartialEq)]
    pub struct PictochatUserMessageEvent {
        pub mac_address: Option<MacAddress>,
        pub message_content: Vec<u8>
    }



    #[derive(Clone, PartialEq, Debug)]
    pub enum PictochatAppState {
        Idle,
        NewClient,
        IdentConsole((MacAddress,ConsoleIdPayload)),
        RequestIdent(u16),
        Echo,
        TXTransfer,
    }

    #[derive(Clone)]
    pub struct PictochatStateObject {
        desired_state: PictochatAppState,
        payload: Option<Vec<u8>>,
        counter: u16,
        offset: u16,
    }

    pub struct PictochatAppDriver {
        //todo: add shit here
        pub i: i32,
        pub seq_no: u16,
        pub next_pkt: Option<Vec<u8>>,
        pub state: PictochatAppState,
        pub member_list: [MacAddress; 16],
        pub desired_state_queue: VecDeque<PictochatStateObject>,
        pub state_tracker: Option<PictochatStateObject>,
        pub temp_buffer: Option<Vec<u8>>,
        pub debounce_timer: Instant,
        pub read_offset: usize,
        pub current_note: u8,
        pub last_frame_time: Instant,
        pub incoming_events: Option<Arc<Mutex<Receiver<PictochatEvent>>>>,
        pub outgoing_events: Option<SyncSender<PictochatEvent>>,
        pub console_state_map: HashMap<MacAddress,ConsoleIdPayload>,
        pub identity: ConsoleIdPayload,
    }

    impl Default for PictochatAppDriver {
        fn default() -> Self {
            Self {
                i: 0,
                seq_no: 0,
                next_pkt: None,
                state: PictochatAppState::Idle,
                state_tracker: None,
                member_list: [MacAddress::from_bytes(&[0u8;6]).unwrap();16],
                desired_state_queue: VecDeque::new(),
                temp_buffer: None,
                debounce_timer: Instant::now(),
                read_offset: 0,
                current_note: 0x0,
                last_frame_time: Instant::now(),
                incoming_events: None,
                outgoing_events: None,
                console_state_map: HashMap::new(),
                identity: ConsoleIdPayload::default(),
            }
        }
    }

    impl PictochatAppDriver {
        fn get_member_list(&self, upper_layer: &impl LocalWifiLayer) -> [MacAddress; 16] {
            self.member_list
        }
        fn set_state(&mut self, new_state: PictochatAppState) {
            //info!("State is now {:?}",new_state);
            self.state = new_state;
        }
    }

    impl LocalWifiApplicationLayer for PictochatAppDriver {
        fn setup(&mut self, upper_layer: &mut impl LocalWifiLayer) {
            upper_layer.set_broadcast_packet(
                Duration::from_millis(100),
                vec![0x48, 0x23, 0x11, 0x0a, 0x01, 0x01, 0x04, 0x00], //todo: make a struct for this packet
                [0x00, 0x00, 0x00, 0x00],
                BeaconType::MULTICART,
                0x00c0,
                0x00c0,
            );
            upper_layer.set_broadcast_enabled(true);
            self.member_list[0] = upper_layer.get_local_mac();
            self.identity.to = upper_layer.get_local_mac();
            self.identity.colour = 00;
            self.identity.bio = WString::from("Hello World!").into();
            self.identity.name = WString::from("").into();

            //info!("setting broadcast enabled");
        }


        fn on_packet(&mut self, upper_layer: &mut impl LocalWifiLayer, data_frame: &mut ClientToHostDataFrame, device: &LocalWifiClientDevice) {
            if self.member_list[device.assoc_id_num as usize] != device.addr {
                self.member_list[device.assoc_id_num as usize] = device.addr.clone();
                info!("not seen {:?}",device.addr.clone());
            }
            let mut frame = data_frame.payload.as_ref().unwrap().clone();

            //info!("packet, type: {}, {}", frame[0], frame.len());
            if frame[0] == 6 {
                self.desired_state_queue.push_back(PictochatStateObject {
                    desired_state: PictochatAppState::NewClient,
                    payload: None,
                    counter: 0,
                    offset: 0,
                });
            } else if frame[0] == 0 {
                let parsed_packet = PictochatType1::from_bytes(&frame);
                //info!("got a message {}",parsed_packet.data_size);
                info!("got a type1 packet for console: {} size {}",parsed_packet.console_id,parsed_packet.data_size);
                if parsed_packet.data_size <= MAX_PACKET_ALLOC_SIZE {
                    self.temp_buffer = Some(vec![0u8; parsed_packet.data_size as usize])
                }
                frame[0] = 1;
                self.desired_state_queue.push_back(PictochatStateObject {
                    desired_state: PictochatAppState::Echo,
                    payload: Some(frame),
                    counter: 0,
                    offset: 0,
                });

            } else if frame[0] == 2 /*|| frame[0] == 3*/ {
                if self.temp_buffer.is_some() {
                    let parsed_packet = PictochatType2::from_bytes(&frame);
                    if let data = parsed_packet.payload.unwrap() {
                        let mut buffer = self.temp_buffer.take().unwrap();
                        //info!("got part of a large struct {}",parsed_packet.write_offset);
                        buffer[parsed_packet.write_offset as usize..][..data.len()].clone_from_slice(&data);
                        self.temp_buffer = Some(buffer);
                    }
                    if parsed_packet.transfer_flags == 0x01 {
                        let buffer = self.temp_buffer.take().unwrap();
                        //info!("got a full large struct, first byte {}, size {}",buffer[0],buffer.len());

                        if buffer[1] == 1 || buffer[1] == 0 {
                            //console id
                            let console_id = ConsoleIdPayload::from_bytes(&buffer);
                            info!("parsed packet as client, {} joining, bio: {}",console_id.name.0.to_string(),console_id.bio.0.to_string());
                            if self.outgoing_events.is_some() {
                                self.outgoing_events.clone().unwrap().try_send(PictochatEvent::STATE(PictochatUserStateEvent {
                                    mac_address: device.addr.clone(),
                                    is_leaving: false,
                                    birthday: console_id.birth_day,
                                    birthmonth: console_id.birth_month,
                                    name: console_id.name.0,
                                    bio: console_id.bio.0,
                                })).expect("TODO: panic message");
                                if !self.console_state_map.contains_key(&device.addr.clone()) {
                                    info!("not seen {:?} before",device.addr.clone());
                                    let console_id_test = ConsoleIdPayload::from_bytes(&buffer);
                                    self.console_state_map.insert(device.addr.clone(), console_id_test);
                                    self.desired_state_queue.push_back(PictochatStateObject {
                                        desired_state: PictochatAppState::Idle,
                                        payload: None,
                                        counter: 0,
                                        offset: 0,
                                    });
                                    self.desired_state_queue.push_back(PictochatStateObject {
                                        desired_state: PictochatAppState::IdentConsole((upper_layer.get_local_mac(),self.identity.clone())),
                                        payload: None,
                                        counter: 0,
                                        offset: 0,
                                    });
                                    self.desired_state_queue.push_back(PictochatStateObject {
                                        desired_state: PictochatAppState::Idle,
                                        payload: None,
                                        counter: 0,
                                        offset: 0,
                                    });
                                    for i in 1..=self.console_state_map.len() {
                                        self.desired_state_queue.push_back(PictochatStateObject {
                                            desired_state: PictochatAppState::RequestIdent(i as u16),
                                            payload: None,
                                            counter: 0,
                                            offset: 0,
                                        });
                                    }


                                    /*for console in self.console_state_map.clone() {
                                        self.desired_state_queue.push_back(PictochatStateObject {
                                            desired_state: PictochatAppState::IdentConsole(console),
                                            payload: None,
                                            counter: 0,
                                            offset: 0,
                                        });
                                    }*/
                                }


                            }
                        } else {
                            let message_packet = MessagePayload::from_bytes(&buffer);
                            if self.outgoing_events.is_some() {
                                self.outgoing_events.clone().unwrap().try_send(PictochatEvent::MESSAGE(PictochatUserMessageEvent {
                                    mac_address: Some(device.addr.clone()),
                                    message_content: message_packet.message,
                                })).expect("TODO: panic message");
                            }
                            //info!("got following packet from client {}",hex::encode(&buffer));
                        }

                        //self.temp_buffer = Some(buffer);
                    }
                }

                self.desired_state_queue.push_back(PictochatStateObject {
                    desired_state: PictochatAppState::Echo,
                    payload: Some(frame),
                    counter: 0,
                    offset: 0,
                });
            }
        }


        fn fill_next_packet(&mut self, upper_layer: &mut impl LocalWifiLayer) {
            self.seq_no+=1;

            if self.incoming_events.is_some() {
                let chan_m = self.incoming_events.clone().unwrap();
                let chan = chan_m.lock().unwrap();
                if let Ok(data) = chan.recv_timeout(Duration::from_millis(10)) {
                    match data {
                        PictochatEvent::STATE(_) => {panic!("Sending a state event to pictochat driver is unsupported")}
                        PictochatEvent::MESSAGE(msg) => {
                            let payload = MessagePayload {
                                from: upper_layer.get_local_mac(),

                                message: msg.message_content,
                                ..Default::default()
                            }.to_bytes();

                            self.desired_state_queue.push_back(PictochatStateObject {
                                desired_state: PictochatAppState::TXTransfer,
                                payload: Some(payload),
                                counter: 0,
                                offset: 0,
                            });
                        }
                    }

                }
            }

            if self.state == PictochatAppState::Idle && !self.desired_state_queue.is_empty() {
                self.state_tracker = self.desired_state_queue.pop_front();
                self.state = self.state_tracker.clone().unwrap().desired_state;
                info!("state is now: {:?}",self.state);
            }

            match &self.state {
                PictochatAppState::Idle => {
                    //whenever there is no other data just send out a member list
                    let mut data = [0u8; 4];
                    rand::thread_rng().fill_bytes(&mut data);
                    upper_layer.set_next_data_frame(HostToClientDataFrame {
                        us_per_client_reply: 998,
                        rx_console_mask: 2,
                        flags: HostToClientFlags::from_bits(28).unwrap(),
                        broadcast_cycles_remaining: 0,
                        payload: Some(
                            PictochatType5 {
                                magic: data,
                                member_list: self.get_member_list(upper_layer),
                                ..Default::default()
                            }.to_bytes(),
                        ),
                        footer: Option::from(HostToClientDataFrameFooter {
                            seq_no: self.seq_no,
                            idk: 0,
                        }),
                    });
                }
                PictochatAppState::NewClient => {
                    // we have a new client, send this packet unsure what it does but its needed for the ds to identify itself
                    let mut data = [0u8; 4];
                    rand::thread_rng().fill_bytes(&mut data);
                    upper_layer.set_next_data_frame(HostToClientDataFrame {
                        us_per_client_reply: 998,
                        rx_console_mask: 2,
                        flags: HostToClientFlags::from_bits(28).unwrap(),
                        broadcast_cycles_remaining: 0,
                        payload: Some(
                            PictochatType4 {
                                magic: data,
                                member_list: self.get_member_list(upper_layer),
                                ..Default::default()
                            }.to_bytes(),
                        ),
                        footer: Option::from(HostToClientDataFrameFooter {
                            seq_no: self.seq_no,
                            idk: 0,
                        }),
                    });
                    self.state = PictochatAppState::Idle;
                }
                PictochatAppState::RequestIdent(console_id) => {
                    upper_layer.set_next_data_frame(HostToClientDataFrame {
                        us_per_client_reply: 998,
                        rx_console_mask: 2,
                        flags: HostToClientFlags::from_bits(29).unwrap(),
                        broadcast_cycles_remaining: 0,
                        payload: Some(
                            PictochatType1 {
                                console_id: *console_id,
                                data_size: 84,
                                ..Default::default()
                            }.to_bytes()
                        ),
                        footer: Option::from(HostToClientDataFrameFooter {
                            seq_no:  self.seq_no,
                            idk: 0,
                        }),
                    });
                    self.state = PictochatAppState::Idle;
                }
                PictochatAppState::IdentConsole((mac,console_id_b)) => {
                    let mut state_tracker = self.state_tracker.take().unwrap();
                    let mut console_id = console_id_b.clone();
                    console_id.magic = [0x03, if state_tracker.counter == 1 {0x00} else {0x01}];
                    info!("IdentConsole stage {}",state_tracker.counter);

                    if state_tracker.counter == 0 || state_tracker.counter == 2 {
                        upper_layer.set_next_data_frame(HostToClientDataFrame {
                            us_per_client_reply: 998,
                            rx_console_mask: 2,
                            flags: HostToClientFlags::from_bits(29).unwrap(),
                            broadcast_cycles_remaining: 0,
                            payload: Some(
                                PictochatType1 {
                                    console_id: 0,
                                    data_size: 84,
                                    ..Default::default()
                                }.to_bytes()
                            ),
                            footer: Option::from(HostToClientDataFrameFooter {
                                seq_no:  self.seq_no,
                                idk: 0,
                            }),
                        });
                    } else if state_tracker.counter == 1 || state_tracker.counter == 3{
                        upper_layer.set_next_data_frame(HostToClientDataFrame {
                            us_per_client_reply: 998,
                            rx_console_mask: 2,
                            flags: HostToClientFlags::from_bits(30).unwrap(),
                            broadcast_cycles_remaining: 0,
                            payload: Some(
                                PictochatType2 {
                                    sending_console_id: 0,
                                    payload_type: 5,
                                    payload_length: 84,
                                    transfer_flags: 1,
                                    write_offset: 0,
                                    payload: Some(console_id.to_bytes()),
                                    /*payload: Some(ConsoleIdPayload {
                                        magic: [0x03, if state_tracker.counter == 1 {0x00} else {0x01}],
                                        to: mac.clone(),
                                        name: console_id.name,
                                        bio: console_id.bio,
                                        colour: console_id.colour,
                                        birth_day: console_id.birth_day,
                                        birth_month: console_id.birth_month,
                                        ..Default::default()
                                    }.to_bytes()),*/
                                    ..Default::default()
                                }.to_bytes()
                            ),
                            footer: Option::from(HostToClientDataFrameFooter {
                                seq_no:  self.seq_no,
                                idk: 0,
                            }),
                        });
                    }

                    if state_tracker.counter == 3 {
                        self.state = PictochatAppState::Idle;
                    } else {
                        state_tracker.counter += 1;
                        self.state_tracker = Some(state_tracker);
                    }
                }
                PictochatAppState::Echo => {
                    let mut state_tracker = self.state_tracker.take().unwrap();
                    let data= state_tracker.payload.take().unwrap();
                    upper_layer.set_next_data_frame(HostToClientDataFrame {
                        us_per_client_reply: 998,
                        rx_console_mask: 2,
                        flags: HostToClientFlags::from_bits(if data.len() > 25 {158} else {29}).unwrap(),
                        broadcast_cycles_remaining: 0,
                        payload: Some(
                            data
                        ),
                        footer: Option::from(HostToClientDataFrameFooter {
                            seq_no:  self.seq_no,
                            idk: 0,
                        }),
                    });
                    self.state = PictochatAppState::Idle;
                }
                PictochatAppState::TXTransfer => {
                    let mut state_tracker = self.state_tracker.take().unwrap();
                    let data= state_tracker.payload.take().unwrap();
                    let total_chunks = data.len().div_ceil(MESSAGE_CHUNK_SIZE as usize) as u16;
                    if state_tracker.counter == 0  {
                        upper_layer.set_next_data_frame(HostToClientDataFrame {
                            us_per_client_reply: 998,
                            rx_console_mask: 2,
                            flags: HostToClientFlags::from_bits(29).unwrap(),
                            broadcast_cycles_remaining: 0,
                            payload: Some(
                                PictochatType1 {
                                    console_id: 0x0000,
                                    data_size: data.len() as u16,
                                    magic_2: [0x00, 0x00,
                                        0x58, 0x2b, 0x00, 0x03,
                                        0xdb, 0xa2, 0xfa, 0xea],
                                    ..Default::default()
                                }.to_bytes()
                            ),
                            footer: Option::from(HostToClientDataFrameFooter {
                                seq_no:  self.seq_no,
                                idk: 0,
                            }),
                        });
                        self.last_frame_time = Instant::now();
                    } else if state_tracker.counter <= total_chunks  {
                        let data_size = if data.len() as u16 - state_tracker.offset > MESSAGE_CHUNK_SIZE as u16 { MESSAGE_CHUNK_SIZE as u16 } else { data.len() as u16 - state_tracker.offset  } as u16;
                        let sub_data = &data[state_tracker.offset as usize..][..data_size as usize];

                        upper_layer.set_next_data_frame(HostToClientDataFrame {
                            us_per_client_reply: 998,
                            rx_console_mask: 2,
                            flags: if state_tracker.counter == total_chunks { HostToClientFlags::from_bits(158).unwrap() } else { HostToClientFlags::from_bits(30).unwrap() },
                            broadcast_cycles_remaining: 0,
                            payload: Some(
                                PictochatType2 {
                                    sending_console_id: 0,
                                    payload_type: if state_tracker.counter == total_chunks { 0x04 } else { if state_tracker.counter == 1 { 0xff } else { 0x97 } },
                                    payload_length: data_size as u8,
                                    transfer_flags: if state_tracker.counter == total_chunks { 0x01 } else { 0x00 },
                                    write_offset: state_tracker.offset,
                                    payload: Some(sub_data.to_vec()),
                                    ..Default::default()
                                }.to_bytes()
                            ),
                            footer: Option::from(HostToClientDataFrameFooter {
                                seq_no:  self.seq_no,
                                idk: 0,
                            }),
                        });
                        if state_tracker.counter == total_chunks {
                            let time = Instant::now() - self.last_frame_time;
                            info!("Frametime: {}ms",time.as_millis());
                        }
                        state_tracker.offset += data_size ;
                    } else {
                        let mut data: [u8; 0x14] = [
                            0x03, 0x00, 0x14, 0x00, 0x01, 0x04, 0xFF, 0xFF, 0x81, 0x93, 0x32, 0x02, 0x01, 0x69, 0x35, 0x02,
                            0x6F, 0x02, 0x33, 0x02
                        ];
                        //data[16..].fill(0);
                        upper_layer.set_next_data_frame(HostToClientDataFrame {
                            us_per_client_reply: 998,
                            rx_console_mask: 2,
                            flags: HostToClientFlags::from_bits(141).unwrap(),
                            broadcast_cycles_remaining: 0,
                            payload: Some(
                                data.to_vec()
                            ),
                            footer: Option::from(HostToClientDataFrameFooter {
                                seq_no:  self.seq_no,
                                idk: 0,
                            }),
                        });
                    }

                    state_tracker.payload = Some(data);
                    if state_tracker.counter == total_chunks  {
                        //info!("returning to idle after large transfer");
                        self.state = PictochatAppState::Idle;
                    } else {
                        state_tracker.counter += 1;
                        self.state_tracker = Some(state_tracker);
                    }
                }

            }

        }

        fn on_client_event(&mut self, client_event: ClientEvents) {
            match client_event {
                ClientEvents::NewClient(_) => {}
                ClientEvents::LostClient((mac,_aid)) => {
                    self.member_list.iter_mut().for_each(|x| {
                        if *x == mac {
                            *x = MacAddress::from_bytes(&[0u8; 6]).unwrap();
                        }
                    });
                }
            }
        }
    }

    fn is_zero(buf: &[u8]) -> bool {
        let (prefix, aligned, suffix) = unsafe { buf.align_to::<u128>() };

        prefix.iter().all(|&x| x == 0)
            && suffix.iter().all(|&x| x == 0)
            && aligned.iter().all(|&x| x == 0)
    }
}
