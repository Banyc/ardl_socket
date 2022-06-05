use ardl::utils::buf::{BufSlice, BufWtr};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::io::Cursor;

pub struct FrameHdr {
    id: u32,
    // body: BufSlice,
}

pub const FRAME_HDR_LEN: usize = 4;

impl FrameHdr {
    fn check_rep(&self) {}

    pub fn from_slice(slice: &mut BufSlice) -> Result<Self, DecodingError> {
        let mut rdr = Cursor::new(slice.data());
        let id = rdr
            .read_u32::<BigEndian>()
            .map_err(|_| DecodingError::NotEnoughSpace)?;
        let read_len = rdr.position() as usize;
        drop(rdr);
        slice.pop_front(read_len).unwrap();
        // let body = slice
        //     .pop_front(slice.len())
        //     .map_err(|_| DecodingError::NotEnoughSpace)?;
        // let this = Frame { id, body };
        let this = FrameHdr { id };
        this.check_rep();
        Ok(this)
    }

    pub fn append_to(&self, wtr: &mut impl BufWtr) -> Result<(), EncodingError> {
        let mut hdr = Vec::new();
        hdr.write_u32::<BigEndian>(self.id).unwrap();
        wtr.append(&hdr)
            .map_err(|_| EncodingError::NotEnoughSpace)?;
        // wtr.append(self.body.data())
        //     .map_err(|_| EncodingError::NotEnoughSpace)?;
        Ok(())
    }

    pub fn id(&self) -> u32 {
        self.id
    }
}

pub struct FrameBuilder {
    pub id: u32,
    // pub body: BufSlice,
}

impl FrameBuilder {
    pub fn build(self) -> FrameHdr {
        let this = FrameHdr {
            id: self.id,
            // body: self.body,
        };
        this.check_rep();
        this
    }
}

#[derive(Debug)]
pub enum EncodingError {
    NotEnoughSpace,
}

#[derive(Debug)]
pub enum DecodingError {
    NotEnoughSpace,
}
