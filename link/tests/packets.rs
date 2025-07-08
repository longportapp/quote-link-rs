use deku::{DekuContainerRead, DekuContainerWrite};

use link::packets::{Command, Packet, Request};

#[test]
fn packets() {
    let magic_num: u32 = 131328;
    println!("magic num le is {:?}", magic_num.to_le_bytes());
    println!("magic num be is {:?}", magic_num.to_be_bytes());

    let packet = Packet::Request(Request {
        reserved: 0,
        command: Command::Heartbeat,
        id: 1,
        timeout: 2,
        body_len: 3,
        body: vec![0x00, 0x01, 0x02],
    });

    let data = packet.to_bytes().unwrap();
    let expect = vec![
        // ty, res, cmd
        0b0000_0001,
        // request_id, 大端序的数据, 以字节序存储后...为此
        0x00,
        0x00,
        0x01,
        // timeout
        0x02,
        // V2 协议
        // 8 bytes timestamp in nano
        // 0x00,
        // 0x00,
        // 0x00,
        // 0x00,
        // 0x00,
        // 0x00,
        // 0x00,
        // 0x00,
        // body len
        0x00,
        0x00,
        0x03,
        // body
        0x00,
        0x01,
        0x02,
        // 多出来的
        0x10,
        0x11,
        0x12,
    ];
    println!("expect: {expect:?}");
    assert_eq!(data, expect[..(expect.len() - 3)]);

    let (rest, val) = Packet::from_bytes((expect.as_ref(), 0)).unwrap();
    println!("{:?}", rest);
    assert_eq!(val, packet);
    assert_eq!(&expect[(expect.len() - 3)..], rest.0);
}

#[test]
fn mut_ref_num() {
    let mut a = 0;
    change_in_place(&mut a);

    assert_eq!(1, a);
}

fn change_in_place(n: &mut i32) {
    *n += 1;
}

#[test]
fn buf_size() {
    let buf: [u8; 1024] = [0; 1024];
    println!("buf len: {}", buf.len());
    println!("buf len after slice: {}", buf[..0].len());
    println!("buf len after slice: {}", buf[..1].len());
}
