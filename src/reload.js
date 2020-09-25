const address = "ws://localhost:9000";
const reload_key = 'fileshare-dev-reload-token';
var socket = new WebSocket(address);
function milliseconds(t) {
	return new Promise(resolve => {
		setTimeout(() => {
			resolve('resolved');
		}, t);
	});
}

function on_message(event) {
	let data = JSON.parse(event.data);
	console.log(data);
	if (data.RefreshPage) {
		let refresh_token = JSON.parse(localStorage.getItem(reload_key));
		if (data.RefreshPage !== refresh_token) {
			localStorage.setItem(reload_key, JSON.stringify(data.RefreshPage));
			location.reload();
		} else {
			console.log("Don't need to reload again.");
		}
	}
}
function on_error(event) {}

async function on_close(event) {
	console.log("Reload server connection closed. Retrying...");
	socket.close();
	socket = new WebSocket(address);
	init_socket(socket);
}

function init_socket(socket) {
	socket.addEventListener('close', on_close);
	socket.addEventListener('message', on_message);
	socket.addEventListener('error', on_error);	
}

init_socket(socket);
