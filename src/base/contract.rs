use soroban_sdk::{contract, contractimpl, Env};

use super::*;

pub trait FluxityTrait {
    fn get_latest_stream_id(e: Env) -> u64;
    fn get_stream(e: Env, id: u64) -> Result<types::StreamType, errors::CustomErrors>;
    fn create_stream(e: Env, params: types::StreamInputType) -> Result<u64, errors::CustomErrors>;
    fn cancel_stream(e: Env, id: u64) -> Result<(i128, i128), errors::CustomErrors>;
    fn withdraw_stream(e: Env, id: u64, amount: i128) -> Result<i128, errors::CustomErrors>;
    fn create_vesting(e: Env, params: types::VestingInputType)
        -> Result<u64, errors::CustomErrors>;
}

#[contract]
pub struct Fluxity;

#[contractimpl]
impl FluxityTrait for Fluxity {
    /// Returns the latest stream id
    ///
    /// # Examples
    ///
    /// ```
    /// let id = fluxity_client::get_latest_stream_id();
    /// ```
    fn get_latest_stream_id(e: Env) -> u64 {
        storage::get_latest_stream_id(&e)
    }

    /// Returns an stream by id
    ///
    /// # Examples
    ///
    /// ```
    /// let stream_id = 20;
    ///
    /// fluxity_client::get_stream(&stream_id);
    /// ```
    fn get_stream(e: Env, id: u64) -> Result<types::StreamType, errors::CustomErrors> {
        match e.storage().persistent().get(&data_key::DataKey::Stream(id)) {
            None => Err(errors::CustomErrors::StreamNotFound),
            Some(stream) => Ok(stream),
        }
    }

    /// Creates an stream
    ///
    /// # Examples
    ///
    /// ```
    /// let params = LinearStreamInputType {
    ///     sender: Address::random(&env),
    ///     receiver: Address::random(&env),
    ///     token: Address::random(&env),
    ///     amount: 20000000,
    ///     start_date: now,
    ///     cancellable_date: now,
    ///     cliff_date: now + 100,
    ///     end_date: now + 1000,
    ///     rate: Rate::Daily
    /// };
    ///
    /// fluxity_client::create_stream(&params);
    /// ```
    fn create_stream(e: Env, params: types::StreamInputType) -> Result<u64, errors::CustomErrors> {
        params.sender.require_auth();

        if params.amount <= 0 {
            return Err(errors::CustomErrors::InvalidAmount);
        }

        if &params.sender == &params.receiver {
            return Err(errors::CustomErrors::InvalidReceiver);
        }

        if &params.start_date >= &params.end_date {
            return Err(errors::CustomErrors::InvalidStartDate);
        }

        if &params.cancellable_date > &params.end_date {
            return Err(errors::CustomErrors::InvalidCancellableDate);
        }

        if &params.cliff_date < &params.start_date || &params.cliff_date > &params.end_date {
            return Err(errors::CustomErrors::InvalidCliffDate);
        }

        token::transfer_from(&e, &params.token, &params.sender, &params.amount);

        let id = storage::get_latest_stream_id(&e);
        let stream: types::StreamType = params.into();

        storage::set_stream(&e, id, &stream);
        storage::increment_latest_stream_id(&e, &id);
        events::publish_stream_created_event(&e, id);

        Ok(id)
    }

    /// Cancels an stream
    ///
    /// # Examples
    ///
    /// ```
    /// let stream_id = 20;
    ///
    /// fluxity_client::cancel_stream(&stream_id);
    /// ```
    fn cancel_stream(e: Env, id: u64) -> Result<(i128, i128), errors::CustomErrors> {
        let mut stream = storage::get_stream_by_id(&e, &id).unwrap();

        stream.sender.require_auth();

        if stream.is_cancelled {
            return Err(errors::CustomErrors::StreamAlreadyCanceled);
        }

        let current_date = e.ledger().timestamp();

        if stream.end_date <= current_date {
            return Err(errors::CustomErrors::StreamAlreadySettled);
        }

        if stream.cancellable_date > current_date {
            return Err(errors::CustomErrors::StreamNotCancellableYet);
        }

        let mut amounts = utils::calculate_stream_amounts(
            stream.start_date,
            stream.end_date,
            stream.cliff_date,
            current_date,
            stream.amount,
        );

        if stream.is_vesting {
            amounts = utils::calculate_vesting_amounts(
                stream.start_date,
                stream.end_date,
                stream.cliff_date,
                current_date,
                stream.rate,
                stream.amount,
            );
        }

        let sender_amount = amounts.sender_amount;
        let receiver_amount = amounts.receiver_amount - stream.withdrawn;

        stream.is_cancelled = true;
        stream.cancelled_date = current_date;
        stream.withdrawn = amounts.receiver_amount;

        storage::set_stream(&e, id, &stream);

        if receiver_amount > 0 {
            token::transfer(&e, &stream.token, &stream.receiver, &receiver_amount);
        }

        if sender_amount > 0 {
            token::transfer(&e, &stream.token, &stream.sender, &sender_amount);
        }

        events::publish_stream_cancelled_event(&e, id);

        Ok((sender_amount, receiver_amount))
    }

    /// Withdraws from an stream
    ///
    /// # Examples
    ///
    /// ```
    /// let stream_id = 20;
    /// let amount_to_withdraw = 30000000 // Represents 3 in a 7-decimal token
    ///
    /// fluxity_client::withdraw_stream(&stream_id, &amount_to_withdraw);
    /// ```
    fn withdraw_stream(e: Env, id: u64, amount: i128) -> Result<i128, errors::CustomErrors> {
        let mut stream = storage::get_stream_by_id(&e, &id).unwrap();

        if amount < 0 {
            return Err(errors::CustomErrors::AmountUnderflows);
        }

        if stream.is_cancelled {
            return Err(errors::CustomErrors::StreamIsCanceled);
        }

        stream.receiver.require_auth();

        let current_date = e.ledger().timestamp();

        if current_date <= stream.start_date {
            return Err(errors::CustomErrors::StreamNotStartedYet);
        }

        if current_date <= stream.cliff_date {
            return Ok(0);
        }

        let mut amounts = utils::calculate_stream_amounts(
            stream.start_date,
            stream.end_date,
            stream.cliff_date,
            current_date,
            stream.amount,
        );

        if stream.is_vesting {
            amounts = utils::calculate_vesting_amounts(
                stream.start_date,
                stream.end_date,
                stream.cliff_date,
                current_date,
                stream.rate,
                stream.amount,
            );
        }

        let withdrawable = amounts.receiver_amount - stream.withdrawn;

        if withdrawable < amount {
            return Err(errors::CustomErrors::SpecifiedAmountIsGreaterThanWithdrawable);
        }

        let mut amount_to_transfer = amount;

        if amount == 0 {
            amount_to_transfer = withdrawable;
        }

        stream.withdrawn = stream.withdrawn + amount_to_transfer;

        storage::set_stream(&e, id, &stream);

        token::transfer(&e, &stream.token, &stream.receiver, &amount_to_transfer);

        events::publish_stream_withdrawn_event(&e, id);

        Ok(amount_to_transfer)
    }

    /// Creates a vesting stream
    ///
    /// # Examples
    ///
    /// ```
    /// let params = VestingInputType {
    ///     sender: Address::random(&env),
    ///     receiver: Address::random(&env),
    ///     token: Address::random(&env),
    ///     amount: 20000000,
    ///     start_date: now,
    ///     cancellable_date: now,
    ///     cliff_date: now + 100,
    ///     end_date: now + 1000,
    ///     rate: Rate::Daily
    /// };
    ///
    /// fluxity_client::create_vesting(&params);
    /// ```
    fn create_vesting(
        e: Env,
        params: types::VestingInputType,
    ) -> Result<u64, errors::CustomErrors> {
        params.sender.require_auth();

        if params.amount <= 0 {
            return Err(errors::CustomErrors::InvalidAmount);
        }

        if &params.sender == &params.receiver {
            return Err(errors::CustomErrors::InvalidReceiver);
        }

        if &params.start_date >= &params.end_date {
            return Err(errors::CustomErrors::InvalidStartDate);
        }

        if &params.cancellable_date > &params.end_date {
            return Err(errors::CustomErrors::InvalidCancellableDate);
        }

        if &params.cliff_date < &params.start_date || &params.cliff_date > &params.end_date {
            return Err(errors::CustomErrors::InvalidCliffDate);
        }

        token::transfer_from(&e, &params.token, &params.sender, &params.amount);

        let id = storage::get_latest_stream_id(&e);
        let stream: types::StreamType = params.into();

        storage::set_stream(&e, id, &stream);
        storage::increment_latest_stream_id(&e, &id);
        events::publish_vesting_created_event(&e, id);

        Ok(id)
    }
}
