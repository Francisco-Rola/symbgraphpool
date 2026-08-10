use acg_cosmwasm_engine::{Address, ExecutionRequest, TransactionId};
use acg_miniwarehouse_workload::{
    decode_execute_request, MiniWarehouseExecuteMsg, MiniWarehouseMix, MiniWarehouseOperation,
    MiniWarehouseScale, MiniWarehouseWorkloadConfig, MiniWarehouseWorkloadGenerator,
};

fn compact_config() -> MiniWarehouseWorkloadConfig {
    let mut config = MiniWarehouseWorkloadConfig::for_contract(Address::new("miniwarehouse"));
    config.scale = MiniWarehouseScale {
        warehouse_count: 2,
        districts_per_warehouse: 2,
        customers_per_district: 3,
        items_per_warehouse: 8,
    };
    config.min_order_lines = 2;
    config.max_order_lines = 3;
    config.max_order_line_quantity = 2;
    config.seed = 7;
    config
}

#[test]
fn bootstrap_covers_every_configured_record_with_monotonic_transaction_ids() {
    let config = compact_config();
    let expected = config.scale.warehouse_count
        + config.scale.warehouse_count * config.scale.districts_per_warehouse
        + config.scale.warehouse_count
            * config.scale.districts_per_warehouse
            * config.scale.customers_per_district
        + config.scale.warehouse_count * config.scale.items_per_warehouse;
    let mut generator = MiniWarehouseWorkloadGenerator::new(config.clone()).unwrap();
    let bootstrap = generator.bootstrap().unwrap();

    assert_eq!(bootstrap.len() as u64, expected);
    assert_eq!(
        bootstrap.first().unwrap().transaction_id(),
        TransactionId(1)
    );
    assert_eq!(
        bootstrap.last().unwrap().transaction_id(),
        TransactionId(expected)
    );
    assert_eq!(
        bootstrap[0].operation,
        MiniWarehouseOperation::SeedWarehouse
    );
    assert!(bootstrap
        .iter()
        .any(|generated| generated.operation == MiniWarehouseOperation::SeedDistrict));
    assert!(bootstrap
        .iter()
        .any(|generated| generated.operation == MiniWarehouseOperation::SeedCustomer));
    assert!(bootstrap
        .iter()
        .any(|generated| generated.operation == MiniWarehouseOperation::SeedStock));

    let first_district = bootstrap
        .iter()
        .find(|generated| generated.operation == MiniWarehouseOperation::SeedDistrict)
        .unwrap();
    assert_eq!(
        decode_execute_request(&first_district.request).unwrap(),
        MiniWarehouseExecuteMsg::SeedDistrict {
            warehouse_id: 1,
            district_id: 1,
            tax_bps: 50,
            next_order_id: config.first_order_id,
        }
    );
    assert_eq!(
        generator.generate_one().unwrap().transaction_id(),
        TransactionId(expected + 1)
    );
}

#[test]
fn equal_seeds_generate_identical_workloads() {
    let config = compact_config();
    let mut first = MiniWarehouseWorkloadGenerator::new(config.clone()).unwrap();
    let mut second = MiniWarehouseWorkloadGenerator::new(config).unwrap();

    assert_eq!(first.generate(64).unwrap(), second.generate(64).unwrap());
}

#[test]
fn new_order_ids_are_monotonic_per_district() {
    let mut config = compact_config();
    config.scale.warehouse_count = 1;
    config.scale.districts_per_warehouse = 1;
    config.mix = MiniWarehouseMix {
        new_order: 1,
        payment: 0,
        delivery: 0,
        restock: 0,
    };
    let mut generator = MiniWarehouseWorkloadGenerator::new(config).unwrap();
    let generated = generator.generate(3).unwrap();
    let order_ids = generated
        .iter()
        .map(
            |generated| match decode_execute_request(&generated.request).unwrap() {
                MiniWarehouseExecuteMsg::NewOrder { order_id, .. } => order_id,
                other => panic!("expected NewOrder, got {other:?}"),
            },
        )
        .collect::<Vec<_>>();
    assert_eq!(order_ids, vec![1, 2, 3]);
}

#[test]
fn remote_stock_probability_can_force_local_or_remote_lines() {
    let mut local_config = compact_config();
    local_config.mix = MiniWarehouseMix {
        new_order: 1,
        payment: 0,
        delivery: 0,
        restock: 0,
    };
    local_config.remote_stock_probability_bps = 0;
    let mut local = MiniWarehouseWorkloadGenerator::new(local_config).unwrap();
    let local_msg = decode_execute_request(&local.generate_one().unwrap().request).unwrap();
    let MiniWarehouseExecuteMsg::NewOrder {
        warehouse_id,
        lines,
        ..
    } = local_msg
    else {
        panic!("expected NewOrder");
    };
    assert!(lines
        .iter()
        .all(|line| line.supply_warehouse_id == warehouse_id));

    let mut remote_config = compact_config();
    remote_config.mix = MiniWarehouseMix {
        new_order: 1,
        payment: 0,
        delivery: 0,
        restock: 0,
    };
    remote_config.remote_stock_probability_bps = 10_000;
    let mut remote = MiniWarehouseWorkloadGenerator::new(remote_config).unwrap();
    let remote_msg = decode_execute_request(&remote.generate_one().unwrap().request).unwrap();
    let MiniWarehouseExecuteMsg::NewOrder {
        warehouse_id,
        lines,
        ..
    } = remote_msg
    else {
        panic!("expected NewOrder");
    };
    assert!(lines
        .iter()
        .all(|line| line.supply_warehouse_id != warehouse_id));
}

#[test]
fn delivery_mix_bootstraps_a_new_order_then_delivers_that_exact_order() {
    let mut config = compact_config();
    config.scale.warehouse_count = 1;
    config.scale.districts_per_warehouse = 1;
    config.mix = MiniWarehouseMix {
        new_order: 0,
        payment: 0,
        delivery: 1,
        restock: 0,
    };
    let mut generator = MiniWarehouseWorkloadGenerator::new(config).unwrap();

    let first = generator.generate_one().unwrap();
    assert_eq!(first.operation, MiniWarehouseOperation::NewOrder);
    let first_msg = decode_execute_request(&first.request).unwrap();
    let MiniWarehouseExecuteMsg::NewOrder {
        warehouse_id,
        district_id,
        order_id,
        ..
    } = first_msg
    else {
        panic!("expected fallback NewOrder");
    };
    assert_eq!(generator.pending_deliveries(), 1);

    let second = generator.generate_one().unwrap();
    assert_eq!(second.operation, MiniWarehouseOperation::Delivery);
    let MiniWarehouseExecuteMsg::Delivery {
        warehouse_id: delivered_warehouse,
        district_id: delivered_district,
        order_id: delivered_order,
        carrier_id,
    } = decode_execute_request(&second.request).unwrap()
    else {
        panic!("expected Delivery");
    };
    assert_eq!(delivered_warehouse, warehouse_id);
    assert_eq!(delivered_district, district_id);
    assert_eq!(delivered_order, order_id);
    assert!((1..=10).contains(&carrier_id));
    assert_eq!(generator.pending_deliveries(), 0);
}

#[test]
fn generated_requests_target_the_configured_contract() {
    let mut generator = MiniWarehouseWorkloadGenerator::new(compact_config()).unwrap();
    for generated in generator.generate(16).unwrap() {
        let ExecutionRequest::Execute { contract, .. } = generated.request else {
            panic!("MiniWarehouse workload must emit execute requests");
        };
        assert_eq!(contract, Address::new("miniwarehouse"));
    }
}

#[test]
fn invalid_generator_configuration_is_rejected_before_generation() {
    let mut config = compact_config();
    config.remote_stock_probability_bps = 10_001;
    assert!(MiniWarehouseWorkloadGenerator::new(config).is_err());

    let mut config = compact_config();
    config.max_order_lines = 9;
    config.scale.items_per_warehouse = 8;
    assert!(MiniWarehouseWorkloadGenerator::new(config).is_err());

    let mut config = compact_config();
    config.mix = MiniWarehouseMix {
        new_order: 0,
        payment: 0,
        delivery: 0,
        restock: 0,
    };
    assert!(MiniWarehouseWorkloadGenerator::new(config).is_err());
}

#[test]
fn hot_warehouse_probability_can_force_a_single_partition() {
    let mut config = compact_config();
    config.scale.warehouse_count = 3;
    config.hot_warehouse_id = 2;
    config.hot_warehouse_probability_bps = 10_000;
    config.mix = MiniWarehouseMix {
        new_order: 0,
        payment: 1,
        delivery: 0,
        restock: 0,
    };
    let mut generator = MiniWarehouseWorkloadGenerator::new(config).unwrap();
    for generated in generator.generate(32).unwrap() {
        match decode_execute_request(&generated.request).unwrap() {
            MiniWarehouseExecuteMsg::Payment { warehouse_id, .. } => assert_eq!(warehouse_id, 2),
            other => panic!("expected Payment, got {other:?}"),
        }
    }
}
