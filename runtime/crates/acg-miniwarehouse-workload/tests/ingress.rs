use acg_cosmwasm_engine::Address;
use acg_miniwarehouse_workload::{
    MiniWarehouseMix, MiniWarehouseScale, MiniWarehouseWorkloadConfig,
    MiniWarehouseWorkloadGenerator,
};
use acg_validator_sim::{
    BlockProducer, BlockProducerConfig, IngressConfig, Mempool, RateControlledIngress,
};

#[test]
fn generated_workload_flows_through_rate_controlled_ingress_and_fifo_blocks() {
    let mut config = MiniWarehouseWorkloadConfig::for_contract(Address::new("miniwarehouse"));
    config.scale = MiniWarehouseScale {
        warehouse_count: 1,
        districts_per_warehouse: 1,
        customers_per_district: 2,
        items_per_warehouse: 8,
    };
    config.min_order_lines = 1;
    config.max_order_lines = 2;
    config.mix = MiniWarehouseMix {
        new_order: 1,
        payment: 0,
        delivery: 0,
        restock: 0,
    };
    let mut generator = MiniWarehouseWorkloadGenerator::new(config).unwrap();
    let requests = generator.generate_requests(8).unwrap();
    let expected_ids = requests
        .iter()
        .map(|request| request.transaction_id())
        .collect::<Vec<_>>();

    let mut ingress = RateControlledIngress::new(
        IngressConfig {
            transactions_per_second: 4,
        },
        0,
    )
    .unwrap();
    ingress.enqueue_all(requests);
    let mempool = Mempool::default();
    assert_eq!(ingress.pump_until(2_000_000_000, &mempool), 8);

    let mut producer = BlockProducer::fifo(BlockProducerConfig::default()).unwrap();
    let block = producer.produce_next(&mempool);
    let actual_ids = block
        .transactions
        .iter()
        .map(|pending| pending.transaction_id())
        .collect::<Vec<_>>();
    assert_eq!(actual_ids, expected_ids);
    assert!(mempool.is_empty());
}
