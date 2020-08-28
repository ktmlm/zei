use crate::ristretto::{RistrettoScalar, RistrettoPoint, CompressedRistretto};
use crate::jubjub::{JubjubScalar, JubjubGroup};
use crate::bls12_381::{BLSScalar, BLSG1, BLSG2, BLSGt};
use crate::groups::{Scalar, Group};
use utils::serialization::ZeiFromToBytes;
use serde::{Serializer};
use utils::errors::ZeiError;

macro_rules! to_from_bytes_scalar {
 ($t:ident) => {
   impl utils::serialization::ZeiFromToBytes for $t {
    fn zei_to_bytes(&self) -> Vec<u8> {
      let mut v = vec![];
      v.extend_from_slice(&self.to_bytes()[..]);
      v
    }
    fn zei_from_bytes(bytes: &[u8]) -> Result<$t, utils::errors::ZeiError> {
      $t::from_bytes(bytes).map_err(|_| utils::errors::ZeiError::DeserializationError)
    }
   }
 };
}

to_from_bytes_scalar!(RistrettoScalar);
to_from_bytes_scalar!(JubjubScalar);
to_from_bytes_scalar!(BLSScalar);

impl ZeiFromToBytes for CompressedRistretto {
  fn zei_to_bytes(&self) -> Vec<u8> {
    self.0.as_bytes().to_vec()
  }
  fn zei_from_bytes(bytes: &[u8]) -> Result<Self, ZeiError> {
    Ok(CompressedRistretto(curve25519_dalek::ristretto::CompressedRistretto::from_slice(bytes)))
  }
}

serialize_deserialize!(CompressedRistretto);
serialize_deserialize!(RistrettoScalar);
serialize_deserialize!(JubjubScalar);
serialize_deserialize!(BLSScalar);


macro_rules! to_from_bytes_group {
  ($g:ident)  => {
    impl utils::serialization::ZeiFromToBytes for $g {
      fn zei_to_bytes(&self) -> Vec<u8> {
        self.to_compressed_bytes()
      }
      fn zei_from_bytes(bytes: &[u8]) -> Result<$g, utils::errors::ZeiError> {
        $g::from_compressed_bytes(bytes).map_err(|_| utils::errors::ZeiError::SerializationError)
      }
    }
  };
}

to_from_bytes_group!(RistrettoPoint);
to_from_bytes_group!(JubjubGroup);
to_from_bytes_group!(BLSG1);
to_from_bytes_group!(BLSG2);
to_from_bytes_group!(BLSGt);

serialize_deserialize!(RistrettoPoint);
serialize_deserialize!(JubjubGroup);
serialize_deserialize!(BLSG1);
serialize_deserialize!(BLSG2);
serialize_deserialize!(BLSGt);