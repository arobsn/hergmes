use criterion::{Criterion, black_box, criterion_group, criterion_main};
use hergmes::address::ErgoAddress;

const P2PK_ADDRESS: &str = "9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCisr";
const P2SH_ADDRESS: &str = "8sZ2fVu5VUQKEmWt4xRRDBYzuw5aevhhziPBDGB";
const P2S_LONG_ADDRESS: &str = "2Z4YBkDsDvQj8BX7xiySFewjitqp2ge9c99jfes2whbtKitZTxdBYqbrVZUvZvKv6aqn9by4kp3LE1c26LCyosFnVnm6b6U1JYvWpYmL2ZnixJbXLjWAWuBThV1D6dLpqZJYQHYDznJCk49g5TUiS4q8khpag2aNmHwREV7JSsypHdHLgJT7MGaw51aJfNubyzSKxZ4AJXFS27EfXwyCLzW1K6GVqwkJtCoPvrcLqmqwacAWJPkmh78nke9H4oT88XmSbRt2n9aWZjosiZCafZ4osUDxmZcc5QVEeTWn8drSraY3eFKe8Mu9MSCcVU";

fn bench_decode_safe_vs_unsafe(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode_safe_vs_unsafe");

    // P2PK addresses (most common)
    group.bench_function("p2pk_safe", |b| {
        b.iter(|| ErgoAddress::decode(black_box(P2PK_ADDRESS)).unwrap())
    });

    group.bench_function("p2pk_unsafe", |b| {
        b.iter(|| ErgoAddress::decode_unsafe(black_box(P2PK_ADDRESS)).unwrap())
    });

    // P2SH addresses
    group.bench_function("p2sh_safe", |b| {
        b.iter(|| ErgoAddress::decode(black_box(P2SH_ADDRESS)).unwrap())
    });

    group.bench_function("p2sh_unsafe", |b| {
        b.iter(|| ErgoAddress::decode_unsafe(black_box(P2SH_ADDRESS)).unwrap())
    });

    // P2S long addresses (worst case for checksum)
    group.bench_function("p2s_long_safe", |b| {
        b.iter(|| ErgoAddress::decode(black_box(P2S_LONG_ADDRESS)).unwrap())
    });

    group.bench_function("p2s_long_unsafe", |b| {
        b.iter(|| ErgoAddress::decode_unsafe(black_box(P2S_LONG_ADDRESS)).unwrap())
    });

    group.finish();
}

criterion_group!(benches, bench_decode_safe_vs_unsafe);
criterion_main!(benches);
