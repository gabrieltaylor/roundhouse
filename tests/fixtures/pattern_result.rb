class PatternResult
  def deconstruct_keys(keys)
    full = {code: :ok, data: {user: false}}
    keys.nil? ? full : full.slice(*keys)
  end
end
