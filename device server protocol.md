# Device <-> Server Protocol

We need a way for the device and server to communicate and relay tachograph and smart card messages.

## Packet Structure

- 1 byte: '$'
- 2 byte: message type
- 2 byte: message length

Messages can be of several types depending on the message type value

- 0: Ping (device/server)
- 1: Pong (device/server)
- 2: Connection Announce (device)
  - Indicate to the server the device that is connected
  - Request to be added to the queue
- 3: Connection Response (server)
  - Acknowledge connection
  - Either add to the queue or indicate a better time to retry
- 4: Ready (server)
  - Indicate to the device that it is ready and waiting for the authentication process to begin
- 5: Tacho to Card (Device)
- 6: Card to Tacho (Server)
- 7: Abort

Each message type has it's own internal structure

### 0: Ping

Sent to ensure the socket doesn't close prematurely. Either side of the connection can initiate a ping-pong.

Body: no data

### 1: Pong

Sent in response to a ping

Body: no data

### 2: Connection Announce

Device IMEI?
Vehicle reg?
VIN?

### 3: Connection Response

1 byte response code

- 0: Reject
  - Used when the IMEI is to be ignored for whatever reason. Device should use an exponential backoff
- 1: Accept, added to the queue
- 2: Try again later

2 byte retry time (optional)

- Seconds to wait before retrying
- Server uses the recent auth process durations, number of devices in the queue, number of cards in the readers, and number of devices recently turned away to estimate a better time to ask again.

### 4: Ready

???

### 5: Tacho to Card

Raw payload? More info?

### 6: Card to Tacho

Raw payload? More info?

### 7: Abort

Reason?
