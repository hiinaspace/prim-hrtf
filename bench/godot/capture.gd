extends SceneTree
# Renders voices through today's Prim voice path (Godot + patched godot-steam-audio,
# settings copied from prim-identity project/net/avatar.gd) and records the master bus.
# Driven by bench/capture_godot.py, which writes plan.json into $PRIM_HRTF_OUT.

var out := OS.get_environment("PRIM_HRTF_OUT")

func _initialize():
	call_deferred("run")

func make_voice(parent: Node) -> Node:
	# avatar.gd, _ready(): the received-voice SteamAudioPlayer configuration.
	var voice = ClassDB.instantiate("SteamAudioPlayer")
	voice.name = "Voice"
	voice.bus = "Voice"
	voice.distance_attenuation = false
	voice.volume_linear = 0.0
	voice.ambisonics = true
	voice.point_source_binaural = true
	voice.attenuation_filter_db = 0.0
	voice.air_absorption = false
	voice.occlusion = false
	voice.panning_strength = 0.0
	voice.attenuation_model = AudioStreamPlayer3D.ATTENUATION_DISABLED
	parent.add_child(voice)
	return voice

func run():
	var plan = JSON.parse_string(FileAccess.get_file_as_string(out.path_join("plan.json")))
	if plan == null:
		push_error("no plan.json in " + out)
		quit(1)
		return
	root.add_child(ClassDB.instantiate("SteamAudioConfig"))
	var cam := Camera3D.new()
	root.add_child(cam)
	cam.current = true
	cam.add_child(ClassDB.instantiate("SteamAudioListener"))
	var bus := AudioServer.bus_count
	AudioServer.add_bus()
	AudioServer.set_bus_name(bus, "Voice")
	var capture := AudioEffectCapture.new()
	capture.buffer_length = 16.0
	AudioServer.add_bus_effect(0, capture)
	var heads: Array[Node3D] = []
	var voices: Array = []
	for i in range(int(plan["max_voices"])):
		var head := Node3D.new()
		root.add_child(head)
		heads.append(head)
		voices.append(make_voice(head))
	print("SETTINGS ambi=", voices[0].ambisonics, " point=", voices[0].point_source_binaural,
		" refl=", voices[0].reflection, " occ=", voices[0].occlusion, " air=", voices[0].air_absorption,
		" dist=", voices[0].distance_attenuation, " max_db=", voices[0].max_db,
		" rate=", AudioServer.get_mix_rate(), " latency=", AudioServer.get_output_latency())
	await create_timer(1.0).timeout
	for item in plan["renders"]:
		var length := 0.0
		var streams := []
		for k in range(item["voices"].size()):
			var v = item["voices"][k]
			var stream := AudioStreamWAV.load_from_file(v["wav"])
			if stream == null:
				push_error("cannot load " + v["wav"])
				quit(1)
				return
			streams.append(stream)
			length = maxf(length, stream.get_length())
			var az := deg_to_rad(float(v["azimuth"]))
			var el := deg_to_rad(float(v.get("elevation", 0.0)))
			var d := float(v["distance"])
			heads[k].position = Vector3(sin(az) * cos(el) * d, sin(el) * d, -cos(az) * cos(el) * d)
			voices[k].volume_linear = float(v["gain"])
		await create_timer(0.4).timeout
		AudioServer.lock()
		capture.clear_buffer()
		for k in range(streams.size()):
			voices[k].play_stream(streams[k])
		AudioServer.unlock()
		await create_timer(length + 0.6).timeout
		var frames := capture.get_buffer(capture.get_frames_available())
		for k in range(streams.size()):
			voices[k].stop()
		var f := FileAccess.open(out.path_join(item["name"] + ".f32"), FileAccess.WRITE)
		for frame in frames:
			f.store_float(frame.x)
			f.store_float(frame.y)
		f.close()
		print("CAPTURE ", item["name"], " frames=", frames.size())
		await create_timer(0.3).timeout
	var meta := FileAccess.open(out.path_join("godot.json"), FileAccess.WRITE)
	meta.store_string(JSON.stringify({"rate": AudioServer.get_mix_rate(), "engine": Engine.get_version_info()["string"]}))
	meta.close()
	quit()
