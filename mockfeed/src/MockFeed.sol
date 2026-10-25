// mockfeed\src\MockFeed.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

contract MockFeed {
    function decimals() external pure returns (uint8) {
        return 8;
    }

    function latestRoundData()
        external
        view
        returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        )
    {
        return (
            1,
            int256(2000e8),
            block.timestamp,
            block.timestamp,
            1
        );
    }
}