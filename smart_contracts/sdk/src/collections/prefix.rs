pub trait Prefix {
    fn to_bytes(&self) -> Vec<u8>;
}

impl Prefix for &str {
    fn to_bytes(&self) -> Vec<u8> {
        self.as_bytes().into()
    }
}

impl Prefix for String {
    fn to_bytes(&self) -> Vec<u8> {
        self.as_bytes().into()
    }
}

impl Prefix for Vec<u8> {
    fn to_bytes(&self) -> Vec<u8> {
        self.clone()
    }
}