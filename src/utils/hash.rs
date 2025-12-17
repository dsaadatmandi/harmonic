use std::collections::{VecDeque};
use proptest::prelude::*;

const BUZ_HASH_TABLE: [u64; 256] = [
    0x5a397884c8e3300d,
    0x98b8578de2ebd473,
    0xb79fd97301d8e0d7,
    0xdc1409703c8cf0b3,
    0xb90008dd3515b2cf,
    0x2c322a79d2e35d03,
    0xb1635bb68dda618f,
    0xf4c0d55ac434fecc,
    0xd897fb75949d47fc,
    0x7ccf74240bb06796,
    0x3944db4725dceb01,
    0x718b05ac7bbc207b,
    0xf54c4390e84b38d,
    0xd7782b1b9320c2b0,
    0xb61c4eec47554025,
    0x1833ef84895faf03,
    0x64b5077878d74e52,
    0xa2fcc5fd387e3c9f,
    0xdb9b91c274793e35,
    0xbe30573b50abddaf,
    0x1342b18198b46ade,
    0x8ca1a7d05c19e308,
    0x417bf3c91f67bc0c,
    0x999d967af2238d8e,
    0xc7d9c45e17a98efa,
    0x1328cd570f5c3478,
    0xc2d0797abb51c1d5,
    0x317177f50c14218c,
    0x42a69e9454de377e,
    0x713e1fff065c0d09,
    0x688d32b1452fcd5a,
    0x31e7c9e24d217cf2,
    0xae8ac0a72328f532,
    0x79cbe5fea04bbacd,
    0xc5ae03bc752728b8,
    0xa7a3daa3ef6de4f0,
    0xb9e20281128d402,
    0x8a3ab49975023241,
    0x9ecea1158e3ddec9,
    0x338b52a8956c531e,
    0xd46673b6cd823a72,
    0x98ac5cfdd5cace7e,
    0x959b04e590c33850,
    0xfec9ab3f442913a7,
    0x1f4f7f9569229f8e,
    0x61236be4aa3b9dbc,
    0x77076f2f2a5ed1a1,
    0x2313dd7f8fb11a4a,
    0x9cee2e0518617b9e,
    0x6b6ed12e98f54951,
    0x7542d11bc4b32b56,
    0x26d45586a18add1b,
    0x1d3b712b370e4f1a,
    0x543d4ab2b1f0c820,
    0xdf9e1fb97ad94e53,
    0x5128f99c9ab99bd8,
    0xb16432663e13f650,
    0xb852e4702bde5545,
    0xdf07740308ba1990,
    0x966b4e2c5afc2d21,
    0x34b20cfc207751f0,
    0xa002448c4b46a0ec,
    0x25c6fe27cb84bf09,
    0xfdab0091646b800e,
    0xa22286f761f1c8eb,
    0x7a87ae5c48e0d0e2,
    0xa3093a9be66351fe,
    0x6366d12076d64c1e,
    0xf74d377c596660bd,
    0x8499b2007fe5520,
    0x3f7a7a0abd49997,
    0x1f63a58adb1bbae3,
    0x2925b5a397c00d68,
    0xd47328e3e665072f,
    0x5871e37c26b55dba,
    0xb7d4f7680deb440c,
    0x6b014b66a09359a4,
    0xb2d7356ee95a1dc4,
    0x669051cfbb6e8094,
    0x66881104cae4bee7,
    0x98e812bdab6978cd,
    0x4d7dbfa0e0513ce1,
    0x130ffa995985f5b3,
    0x8f3c73fbf52e81b8,
    0xe2984f499b56a13a,
    0xce8ae69d3216a6de,
    0x1ff0c3bfbb50b857,
    0xdd3bba0fa973aab7,
    0x455bbfd097c7be56,
    0x2cae5dea85190821,
    0xb08d86f3babed56e,
    0x59f6bdff0cfb30a1,
    0x3cf0556a440cf450,
    0x4aa48ac28e7f50b8,
    0x1f239e2bcb04b683,
    0x45be975aae449e99,
    0xeddea8f0d41f464,
    0x442f79f629db1b39,
    0x7ed45a1671d03055,
    0x7b20524932dbcb51,
    0x675ca9a2cff5e908,
    0xc00970d44ca3fbc8,
    0x3a4599e26639c64a,
    0x70a564e7a92bf63c,
    0xcc18176d8f4033a3,
    0x7dea59b4dc76738a,
    0xfcf2eeea0201ab95,
    0x1cec2b3f862128e4,
    0x14740781f119ec82,
    0x44c289b8e9c6ba60,
    0xb71b536c058bcc4e,
    0x8315659134b04c3a,
    0x78626f5126da275,
    0x2a79b220963a7796,
    0xd0b9ddc94c9ba7b7,
    0xffe215a1e54d6751,
    0x58b969e13bf30697,
    0x62e9ddb0cfbbe57a,
    0xce2451d71e3b4af0,
    0x837d063a3ca0220f,
    0x2d4b6007d4aaa5f5,
    0x4e2f1795ba4e17d7,
    0x75722402c7ad3a15,
    0x623cd34609d87fc1,
    0x70b11ff93d8f9fd0,
    0x77927bfccc63fb9b,
    0xc6022e872cfd0dd,
    0x358179cfe1f895a9,
    0x957e67d1407c8a7b,
    0x189e91d6321c8dac,
    0x5537b2ae8ee1a33b,
    0x6567ce896c80845d,
    0x5b3af89d848a0cd6,
    0xff68ea0db592a24a,
    0x59afe8996b2af893,
    0x94b7769df739fdfc,
    0x28b0736adb8109e1,
    0x14419e50149d9215,
    0x2ba5b25d742ce13a,
    0x2fa9a5af2949f00d,
    0x1a6e4fcf47a432a2,
    0x662e8f25dcc38587,
    0x15f08f7bd3f9b13f,
    0x798bea071628771,
    0x88103a17fa1490ef,
    0x34dbcd38e4613593,
    0x9d23b5554cb0078b,
    0x30c461e733c0be1e,
    0x66547db8950ddf0b,
    0xa7c8fad4ac0c3225,
    0x4104cf2a76d502e2,
    0x1dc72363486eb8f1,
    0x4caff10ffc674110,
    0xa83b9d179540798,
    0x159767c53bb75c8f,
    0xd0d480bcd70e3d3b,
    0x39805b1547786266,
    0x1de280c254892e82,
    0x231d1e359fb5a0a4,
    0x599d07435981d3c8,
    0x55f2ed2100079d49,
    0xcca9e3e63d32b2f6,
    0xeb933c101bf7ff40,
    0xf9a9fa6e14e425aa,
    0xb67f60286318148f,
    0x3c617fc50250ec13,
    0xe112c657728f5ae9,
    0x9b162353ec52d169,
    0xdc555da692cf0b12,
    0xda2c6c970f13273a,
    0xc30fd652a5f695aa,
    0x63e6abcb94903378,
    0xa3b82696445f716,
    0x682a0a62458effe9,
    0xd1a5ebbc33b50a56,
    0x4c7525f4f3ea37b5,
    0x58918de4bfdd22ed,
    0x89a7c02700714f08,
    0xc34721d5371ff3c,
    0x9e9856e29d459633,
    0xa94a9f8d9b967e96,
    0x14185877f57a7fba,
    0xa0e8ca8be72a5276,
    0xc6585e35f86a67b0,
    0xe9c20d7a49a9e3a7,
    0xf19537e6130b5660,
    0x359420717d18b194,
    0xb45a2c7545e605b4,
    0xbb2f3e16c9e2f4f,
    0xd75461fab1d0b5a5,
    0x78c3069bcf792194,
    0xf93c0086193f52b2,
    0xb579bcb05a092019,
    0xa15c90e1315c3f93,
    0x1c11569ac3f283cc,
    0x42f5952e5981ccc3,
    0x7f206fdb44497a18,
    0x2532535678af58e6,
    0x523f0c2c922ea265,
    0x9e712db14e700724,
    0xd33b901a5e20da53,
    0x5175e63a7a462bf1,
    0x58afceadca83f784,
    0x4adf14b118ea8b0,
    0x1a0d02436dae1dcd,
    0x64f98a9813bcad43,
    0xb27788fd050abcdc,
    0x7be54a6bdef5483b,
    0x32c4222efbc1fc29,
    0xcc960e079dd7c1cf,
    0xefbc3c50acfd11d7,
    0xd474895d06398e5b,
    0xaa37f6aaedee02fe,
    0x7ef80b4f6ac758af,
    0xe6adc70fc4ca7d9f,
    0xdd309db5f335b2d9,
    0x1d1092cd2135a03d,
    0x617a744310ab08ba,
    0xed74fe7a9049165b,
    0xb687331140079278,
    0x102efdd3285a4b12,
    0xbb0ed3f251808f31,
    0x25fe9c32305a43fc,
    0xc4803e660dbb0e8d,
    0x3a515febadae121c,
    0x30f4316cc0bfc23c,
    0x8df4a56fc9678a6f,
    0x5135069c5d6e41f3,
    0x67127720739c77c5,
    0x3868828e331a4ba9,
    0x16444459398bc3a3,
    0xac097b757be40201,
    0x85383d2589ea986e,
    0x3002d5578c615af7,
    0x7b1ac741632235fe,
    0x75a671e340e74dbc,
    0xac0fe4ead80dd56f,
    0x99ac5e59a600908e,
    0x21a35451def12efd,
    0x88dfb63c4b356ffc,
    0x7840d962717988b5,
    0x8c37764ca7a66fea,
    0xa43775d613365401,
    0xe2772843be6b5da7,
    0x73b72873552ffa22,
    0xb4238cf0eee216ef,
    0xc882b27eb9069e79,
    0x6608a425817d5db3,
    0x4192a8e6a2d07d7e,
    0xd69b9ee192b3ec95,
    0xf9c952111cd7c0fa,
    0xaeef6f5f8f76a9fa,
    0xa29980b6f4cd33cc,
    0xfc27281d8ca4ab4d,
    0xdf7679400fff8e20,
    0x42b28c6ebb778ddc,
];

pub struct BuzHash {
    window_size: usize,
    window: VecDeque<u8>,
    pub hash: u64,
}

impl BuzHash {
    pub fn new(window_size: usize) -> Self {
        BuzHash {
            window_size,
            window: VecDeque::with_capacity(window_size),
            hash: 0,
        }
    }

    pub fn compute(&mut self, data: &[u8]) -> u64 {
        self.window.clear();

        for &b in data.iter().take(self.window_size) {
            self.window.push_back(b);
        }

        self.hash()
    }

    fn rotate(x: u64, n: usize) -> u64 {
        // need to calculate performance impact of
        // computing larger hash vs more frequent false positives when lookup up weak checksum
        // there is a case for computing larger hashe if it significantly reduces collision
        let n = n % 64;
        if n == 0 {
            x
        } else {
            (x << n) | (x >> (64 - n))
        }
    }

    fn hash(&mut self) -> u64 {
        self.hash = self.window.iter()
            .enumerate()
            .map(|(n, x)| Self::rotate(BUZ_HASH_TABLE[*x as usize], self.window_size - (1 + n)))
            .fold(0, |acc, value| acc ^ value);

        self.hash
    }

    pub fn roll(&mut self, new_byte: u8) -> u64 {
        let bye = self.window.pop_front().expect("Queue must not be empty at this stage.");

        self.window.push_back(new_byte);

        self.hash = Self::rotate(self.hash, 1);

        self.hash ^= Self::rotate(BUZ_HASH_TABLE[bye as usize], self.window_size);

        self.hash ^= BUZ_HASH_TABLE[new_byte as usize];

        self.hash

    }
}

proptest! {
    #[test]
    fn test_rolling_hash(
        data in prop::collection::vec(any::<u8>(), 100..1000),
        window_size in 1usize..50usize,
    ) {
        let window = &data[data.len()-window_size..];
        let mut hasher = BuzHash::new(window_size);
        let expected_hash = hasher.compute(window);

        let mut rolling_hasher = BuzHash::new(window_size);
        rolling_hasher.compute(&data[0..window_size]);

        for &byte in data[window_size..].iter() {
            rolling_hasher.roll(byte);
        }

        prop_assert_eq!(rolling_hasher.hash, expected_hash);
    }
}