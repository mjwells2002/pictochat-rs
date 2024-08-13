pub mod pictochat_packets {
    use std::io::{Cursor, Read};
    use byteorder::ReadBytesExt;
    use bytes::Buf;
    use ieee80211::MacAddress;
    use log::warn;
    use utf16string::{LittleEndian, WString};
    use crate::pictochat_app_driver::pictochat_app_driver::WStringWrapper;

    pub struct PictochatHeader {
        pub pkt_type: u16,
        pub pkt_size_with_header: u16,
    }

    pub struct PictochatType5 {
        pub hdr:PictochatHeader,
        pub magic: [u8;4],
        pub member_list: [MacAddress; 16],
    }

    impl Default for PictochatType5 {
        fn default() -> Self {
            Self {
                hdr: PictochatHeader {
                    pkt_type: 5,
                    pkt_size_with_header: 104,
                },
                magic: [0xfd, 0x32, 0xea, 0x59],
                member_list: [MacAddress::from_bytes(&[0x0u8,0x0,0x0,0x0,0x0,0x0]).unwrap();16],
            }
        }
    }


    impl PictochatType5 {
        pub fn to_bytes(&self) -> Vec<u8> {
            // current order
            // 0 1 2 3 4 5
            // desired order
            // 1 0 3 2 5 4
            let mut mac_bytes = [0x00u8; 6 * 16];

            for i in 0..16 {
                let bytes = self.member_list[i].as_bytes();
                for o in (0..6).step_by(2) {
                    mac_bytes[(i*6)+o] = bytes[o+1];
                    mac_bytes[(i*6)+o+1] = bytes[o];
                }
            }

            [&self.hdr.pkt_type.to_le_bytes(),
                &self.hdr.pkt_size_with_header.to_le_bytes(),
                self.magic.as_slice(),
                &mac_bytes
                ].concat()
        }
    }

    pub struct PictochatType4 {
        pub hdr:PictochatHeader,
        pub magic: [u8;4],
        pub member_list: [MacAddress; 16],
    }

    impl Default for PictochatType4 {
        fn default() -> Self {
            Self {
                hdr: PictochatHeader {
                    pkt_type: 4,
                    pkt_size_with_header: 104,
                },
                magic: [0x34, 0xfb, 0xad, 0x8c],
                member_list: [MacAddress::from_bytes(&[0x0u8,0x0,0x0,0x0,0x0,0x0]).unwrap();16],

            }
        }
    }


    impl PictochatType4 {
        pub fn to_bytes(&self) -> Vec<u8> {
            // current order
            // 0 1 2 3 4 5
            // desired order
            // 1 0 3 2 5 4
            let mut mac_bytes = [0x00u8; 6 * 16];

            for i in 0..16 {
                let bytes = self.member_list[i].as_bytes();
                for o in (0..6).step_by(2) {
                    mac_bytes[(i*6)+o] = bytes[o+1];
                    mac_bytes[(i*6)+o+1] = bytes[o];
                }
            }

            [&self.hdr.pkt_type.to_le_bytes(),
                &self.hdr.pkt_size_with_header.to_le_bytes(),
                self.magic.as_slice(),
                &mac_bytes
            ].concat()
        }
    }


    /*
     always before a type 2 packet, seems to be a get ready for/data request packet,
     can be sent from host console and get a reply type 2, (followed by the type 2 packet then being echoed)
     or can be sent from host console and be followed by a reply packet
     */


    pub struct PictochatType1 {
        pub hdr: PictochatHeader,
        pub console_id: u16,
        pub magic_1: [u8;2],
        pub data_size: u16,
        pub magic_2: [u8;10],
    }

    impl Default for PictochatType1 {
        fn default() -> Self {
            Self {
                hdr: PictochatHeader {
                    pkt_type: 1,
                    pkt_size_with_header: 20,
                },
                console_id: 0,
                magic_1: [0xff, 0xff],
                data_size: 0,
                magic_2: [0x00, 0x00,
                    0x00, 0x00, 0x00, 0x00,
                    0xb7, 0x78, 0xd5, 0x29],
            }
        }
    }


    impl PictochatType1 {
        pub fn to_bytes(&self) -> Vec<u8> {
            [&self.hdr.pkt_type.to_le_bytes(),
                &self.hdr.pkt_size_with_header.to_le_bytes(),
                &self.console_id.to_le_bytes(),
                self.magic_1.as_slice(),
                &self.data_size.to_le_bytes(),
                self.magic_2.as_slice()].concat()
        }
        pub fn from_bytes(bytes: &Vec<u8>) -> Self {
            let mut pkt = Self {..Self::default()};
            let mut current = Cursor::new(bytes);
            pkt.hdr.pkt_type = current.read_u16::<LittleEndian>().unwrap();
            pkt.hdr.pkt_size_with_header = current.read_u16::<LittleEndian>().unwrap();
            pkt.console_id = current.read_u16::<LittleEndian>().unwrap();
            current.read_exact(&mut pkt.magic_1).expect("TODO: panic message");
            pkt.data_size = current.read_u16::<LittleEndian>().unwrap();
            current.read_exact(&mut pkt.magic_2).expect("TODO: panic message");
            pkt
        }
    }

    pub struct MessagePayload {
        pub magic: u8,
        pub subtype: u8,
        pub from: MacAddress,
        pub magic_1: [u8; 14],
        pub safezone: [u8; 14],
        pub message: Vec<u8>,
    }
    impl Default for MessagePayload {
        fn default() -> Self {
            Self {
                magic: 3,
                subtype: 2,
                from: MacAddress::from_bytes(&[0x0u8,0x0,0x0,0x0,0x0,0x0]).unwrap(),
                magic_1: [0x00u8, 0x05, 0x00, 0x00, 0x00, 0x00, 0x03, 0x06, 0x08, 0x0D, 0x08, 0x0D, 0x12, 0x1B],
                safezone: [0x00u8; 14],
                message: vec![0; 0],
            }
        }
    }

    impl MessagePayload {
        pub fn to_bytes(&self) -> Vec<u8> {
            let mut buf = Vec::new();
            let from_bytes = self.from.as_bytes();
            let mut from_bytes_swapped = [0x00u8; 6];
            for i in (0..6).step_by(2) {
                from_bytes_swapped[i] = from_bytes[i + 1];
                from_bytes_swapped[i + 1] = from_bytes[i];
            }

            buf.push(self.magic);
            buf.push(self.subtype);
            buf.extend_from_slice(&from_bytes_swapped);
            buf.extend_from_slice(&self.magic_1);
            buf.extend_from_slice(&self.safezone);
            buf.extend_from_slice(&self.message);
            buf
        }

        pub fn from_bytes(bytes: &Vec<u8>) -> Self {
            let mut payload = Self::default();
            let mut cursor = std::io::Cursor::new(bytes);

            payload.magic = cursor.get_u8();
            payload.subtype = cursor.get_u8();

            let from_bytes_swapped = cursor.copy_to_bytes(6).to_vec();
            let mut from_bytes = [0x00; 6];
            for i in (0..6).step_by(2) {
                from_bytes[i] = from_bytes_swapped[i + 1];
                from_bytes[i + 1] = from_bytes_swapped[i];
            }

            payload.from = ieee80211::MacAddress::from_bytes(&from_bytes).unwrap();
            payload.magic_1.copy_from_slice(&cursor.copy_to_bytes(14));
            payload.safezone.copy_from_slice(&cursor.copy_to_bytes(14));
            payload.message = cursor.remaining_slice().to_vec();

            payload
        }
    }

    #[derive(Debug,Clone,Eq,PartialEq)]
    pub struct ConsoleIdPayload {
        pub magic: [u8;2],
        pub to: MacAddress,
        pub name: WStringWrapper,
        pub bio: WStringWrapper,
        pub colour: u16,
        pub birth_day: u8,
        pub birth_month: u8,
    }
    impl Default for ConsoleIdPayload {
        fn default() -> Self {
            Self {
                magic: [0x03, 0x00],
                to: MacAddress::from_bytes(&[0x0u8,0x0,0x0,0x0,0x0,0x0]).unwrap(),
                name: WString::from("name").into(),
                bio: WString::from("bio").into(),
                colour: 0,
                birth_day: 1,
                birth_month: 1,
            }
        }
    }

    impl ConsoleIdPayload {
        pub fn to_bytes(&self) -> Vec<u8> {
            // current order
            // 0 1 2 3 4 5
            // desired order
            // 1 0 3 2 5 4
            let to_bytes = self.to.as_bytes();
            let mut to_bytes_swapped = [0x00u8;6];
            for i in (0..6).step_by(2) {
                to_bytes_swapped[i] = to_bytes[i+1];
                to_bytes_swapped[i+1] = to_bytes[i];
            }
            let mut bio_bytes = self.bio.0.as_bytes().to_vec();
            let mut name_bytes = self.name.0.as_bytes().to_vec();;
            name_bytes.resize(20,0x00);
            bio_bytes.resize(52,0x00);
            [
                self.magic.as_slice(),
                &to_bytes_swapped,
                name_bytes.as_slice(),
                bio_bytes.as_slice(),
                &self.colour.to_le_bytes(),
                [self.birth_day,self.birth_month].as_slice()
            ].concat()
        }

        pub fn from_bytes(bytes: &[u8]) -> Self {
            let mut current = Cursor::new(bytes);
            let mut pkt = Self::default();
            current.read_exact(&mut pkt.magic).expect("TODO: panic message");
            let mut macaddr = [0u8;6];
            let mut macaddr_swapped = [0x00u8;6];
            for i in (0..6).step_by(2) {
                macaddr_swapped[i] = macaddr[i+1];
                macaddr_swapped[i+1] = macaddr[i];
            }
            current.read_exact(&mut macaddr_swapped).expect("TODO: panic message");
            pkt.to = MacAddress::from_bytes(&macaddr).unwrap();
            let mut name = vec![0u8;20];
            current.read_exact(&mut name).expect("TODO: panic message");
            pkt.name = WString::from_utf16le(name).unwrap().into();
            let mut bio = vec![0u8;52];
            current.read_exact(&mut bio).expect("TODO: panic message");
            pkt.bio = WString::from_utf16le(bio).unwrap().into();
            pkt.colour = current.read_u16::<LittleEndian>().unwrap();
            pkt.birth_day = current.read_u8().unwrap();
            pkt.birth_month = current.read_u8().unwrap();

            pkt
        }
    }

    /*
    imhex struct
    bitfield transfer_flags {
        end_of_transfer : 1;
        padding:7;
    };

    struct pictochat_pkt {
        u16 type;
        u16 len_with_hdr;
        u8 sending_console_id;
        u8 subtype;
        u8 sublen;
        transfer_flags flags;
        u16 offset;
        padding[2];
        u8 payload[sublen];
    };

    pictochat_pkt pkt @ 0x0;
    seems to be a memcpy type packet
    always preceded by a type 1 packet


    */

    pub struct PictochatType2 {
        pub hdr:PictochatHeader,
        pub sending_console_id: u8,
        pub payload_type: u8,
        pub payload_length: u8,
        pub transfer_flags: u8,
        pub write_offset: u16,
        pub payload: Option<Vec<u8>>,
    }

    impl Default for PictochatType2 {
        fn default() -> Self {
            Self {
                hdr: PictochatHeader {
                    pkt_type: 2,
                    pkt_size_with_header: 96,
                },
                sending_console_id: 0,
                payload_type: 0,
                payload_length: 0,
                transfer_flags: 0,
                write_offset: 0,
                payload: None,
            }
        }
    }

    impl PictochatType2 {
        pub fn to_bytes(&self) -> Vec<u8> {
            [   &self.hdr.pkt_type.to_le_bytes(),
                &self.hdr.pkt_size_with_header.to_le_bytes(),
                &[self.sending_console_id,self.payload_type],
                &[self.payload_length,self.transfer_flags],
                &self.write_offset.to_le_bytes(),
                &[0x00u8,0x00],
                self.payload.clone().unwrap_or_default().as_slice(),
            ].concat()
        }
        pub fn from_bytes(bytes: &Vec<u8>) -> Self {
            let mut pkt = Self {..Self::default()};
            let mut current = Cursor::new(bytes);
            pkt.hdr.pkt_type = current.read_u16::<LittleEndian>().unwrap();
            pkt.hdr.pkt_size_with_header = current.read_u16::<LittleEndian>().unwrap();
            pkt.sending_console_id = current.read_u8().unwrap();
            pkt.payload_type = current.read_u8().unwrap();
            pkt.payload_length = current.read_u8().unwrap();
            pkt.transfer_flags = current.read_u8().unwrap();
            pkt.write_offset = current.read_u16::<LittleEndian>().unwrap();
            let _ = current.read_u16::<LittleEndian>().unwrap(); //seek forward 2 bytes
            if pkt.payload_length != 0 {
                let mut pkt_bytes = vec![0u8; pkt.payload_length as usize];
                match current.read_exact(&mut pkt_bytes) {
                    Ok(_) => {}
                    Err(_) => {
                        warn!("failed to read client to host packet payload");
                    }
                }
                pkt.payload = Some(pkt_bytes);
            }
            pkt
        }
    }
}