// certora/specs/Vault.spec
# certora/specs/Vault.spec
# C6: Proof before profit
# C9: DAO fee accounting (v12)

# C6: Profit only after valid proof AND depth >= 12
rule profit_requires_proof(bytes32 blueprintHash, bytes calldata proof) {
    require confirmation_depth(blueprintHash) < 12;
    releaseProfit@withrevert(blueprintHash, proof);
    assert lastReverted;
}

# C9: DAO fee split integrity
rule dao_fee_accounting(bytes32 blueprintHash, bytes calldata proof) {
    uint256 net = pending_profit(blueprintHash);
    releaseProfit(blueprintHash, proof);
    uint256 dao = dao_fee_address.balance - dao_fee_address.balance@before;
    uint256 pil = pil_treasury.balance - pil_treasury.balance@before;
    assert dao + pil == net;
    assert dao <= net / 10;  # max 10% DAO fee
}
